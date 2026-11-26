//! scc8 (D-H0BZN, D-3RSCW, D-4XHPH, D-5Y8Y1, D-47HVF, D-0SC9C, D-QMZ92, D-0WWYR, D-QCE8M): the
//! sync exchange's push and pull. The push picks the share remote, passes discovery and the
//! state step, checks the hub's minimum and the base rule, looks up a found pending write, then
//! sends canon and each named view under its own push id: known, put, then the write with the
//! render inputs and, for canon, the gate settings. Every test injects the clock, the
//! credential transport, the renderer and the wire; a real project and a real sign-in file stand
//! behind them. The hub refusal mapping runs over the real HTTPS wire against a loopback TLS
//! fixture.
mod common;
#[path = "support/hub_tls.rs"]
mod hub_tls;
#[path = "support/sync_env.rs"]
mod sync_env;

use hub_tls::{Reply, Server};
use rdy_engine::hub_http::HttpHubTransport;
use rdy_engine::hub_publish::{PublishRenderer, RenderedUpload};
use rdy_engine::hub_publish_stream::{
    UploadProbe, with_upload_probe_for_tests, with_upload_worker_setup_for_tests,
};
use rdy_engine::hub_publish_wire::{SYNC_MEDIA_TYPE, UploadContext};
use rdy_engine::hub_sync::{
    CAPTURE_FAILED_FIX, CAPTURE_FAILED_MESSAGE, with_canon_walk_hook_for_tests,
};
use rdy_engine::hub_sync::{
    CanonPreview, PreviewKind, PullRequest, PushOutcome, PushRequest, SyncService, SyncWarning,
    WriteEnd, canon_preview,
};
use rdy_engine::hub_sync_wire::{
    GateSettings, GetRequest, HttpSyncWire, HubRequest, KnownPull, LookupOutcome, ProposeGate,
    SyncStateRequest, SyncWire, WriteFailure, WriteKind, WriteRequest as SyncWrite, WriteTag,
};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::landing_check::CheckInputs;
use rdy_engine::readiness::assessment::pack_digest;
use rdy_engine::render_inputs::RenderInputs;
use rdy_engine::sync_restore::{RestoreOutcome, RestoreRequest};
use rdy_engine::*;
use rdy_model::Hash;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{
    KIND_CANON, KIND_VIEW, SYNC_LOCKED_FIX, SyncCanon, SyncHeld, SyncPending, SyncStep, SyncUpload,
    sync_locked_message,
};
use rdy_store::sync_stream::StreamPurpose;
use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
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

#[track_caller]
fn err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Err(e) => e,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

fn transport_failure() -> RdyError {
    RdyError::new(
        E_UPSTREAM_TRANSPORT,
        UPSTREAM_TRANSPORT_MESSAGE,
        UPSTREAM_TRANSPORT_FIX,
    )
}

/// A hub refusal of `code` in the hub's own words.
fn hub_words(code: &'static str) -> RdyError {
    RdyError::new(code, "the hub's words", "the hub's fix")
}

fn landing(hash: Hash) -> ObjectKey {
    ObjectKey {
        kind: kinds::LANDING,
        hash,
    }
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The canon end's fields of a taken write: push, base, head, tip, resumed and sent.
#[track_caller]
fn taken(out: &PushOutcome) -> (String, Option<Hash>, Hash, Hash, bool, HistorySentView) {
    match &out.canon {
        WriteEnd::Taken {
            push,
            base,
            head,
            tip,
            resumed,
            sent,
            ..
        } => (
            push.clone(),
            *base,
            *head,
            *tip,
            *resumed,
            HistorySentView {
                batches: sent.batches as u64,
                records: sent.records,
            },
        ),
        other => panic!("{other:?}"),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct HistorySentView {
    batches: u64,
    records: u64,
}

#[track_caller]
fn up_to_date(out: &PushOutcome) -> Option<Hash> {
    match &out.canon {
        WriteEnd::UpToDate { tip } => *tip,
        other => panic!("{other:?}"),
    }
}

/// Seeds a sent write of push `push` for head `head` at `at_ms`: its upload, then `Send`.
fn seed_pending(e: &Env, push: &str, head: Hash, at_ms: i64) -> SyncPending {
    let pending = SyncPending {
        repo_id: repo_of(e),
        url: e.url.clone(),
        push: push.into(),
        kind: KIND_CANON,
        view: None,
        expected: None,
        head,
        at_ms,
    };
    seed_sync(
        e,
        "shared",
        vec![
            SyncStep::Upload(SyncUpload {
                repo_id: pending.repo_id,
                url: pending.url.clone(),
                push: push.into(),
                kind: KIND_CANON,
                view: None,
                at_ms,
            }),
            SyncStep::Send(pending.clone()),
        ],
    );
    pending
}

const P1: &str = "0123456789abcdefghjkmnpqrs";
const P2: &str = "7zzzzzzzzzzzzzzzzzzzzzzzzz";

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

/// D-H0BZN's base rule: a null hub tip seeds, a tip local canon holds is the base, and any
/// other tip is `E_DIVERGED` with the exact words before any known or put step. A tip that is
/// local canon's head has nothing to send. The write follows the last put and takes the upload.
#[test]
fn scc8_no_state_push_seeds_extends_or_diverges() {
    // seed: the whole canon history travels, then the write takes it
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let hub = Hub::new();
    let out = push(&e, &hub, None).unwrap();
    let (id, base, pushed, tip, resumed, sent) = taken(&out);
    assert_eq!((base, pushed, tip, resumed), (None, head, head, false));
    // an empty hub holds nothing, so the seed skips the known step
    assert_eq!(hub.steps(), ["discovery", "state", "put", "write"]);
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, None));
    assert_eq!(sent.batches, 1);
    assert_eq!(sent.records, sent_keys.len() as u64);
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.push.as_str(), w.expected, w.view.as_deref()),
        (WriteKind::Landings, id.as_str(), None, None)
    );
    // the write consumed the upload; the reply recorded the head as applied
    assert_eq!(upload_of(&e, "shared"), None);
    assert_eq!(pending_of(&e, "shared"), None);
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!(
        (c.applied, c.held, c.url.as_str(), c.repo_id),
        (Some(head), None, e.url.as_str(), repo_of(&e))
    );

    // extend: a hub tip local canon holds is the base; its closure stays home
    let e = env(3);
    let tip = e.landings[0];
    let head = e.landings[2];
    let hub = Hub::new().with_state(|s| {
        s.canon = Some(tip);
    });
    let out = push(&e, &hub, None).unwrap();
    let (_, base, ..) = taken(&out);
    assert_eq!(base, Some(tip));
    assert_eq!(hub.writes()[0].expected, Some(landing(tip)));
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, Some(tip)));
    assert!(!sent_keys.contains(&landing(tip)));

    // up to date: the hub tip is the head and the hub holds its whole closure
    let hub = Hub::new().with_state(|s| s.canon = Some(head));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    assert_eq!(up_to_date(&push(&e, &hub, None).unwrap()), Some(head));
    // landings with no attachment: the closure above the head is empty, so known is not asked
    assert_eq!(closure(&e, head, Some(head)), BTreeSet::new());
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(upload_of(&e, "shared"), None);

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

/// D-KE83X, D-H0BZN: an attachment made after the hub took local canon's head travels though
/// both tips match. The push asks known for the head's attachments and sends the missing ones in
/// a batch that names the head as a prerequisite; its write expects the head and carries the new
/// tag. It is up to date only when the hub holds every one of them.
#[test]
fn scc8_equal_tip_push_sends_late_attachments() {
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let hub = Hub::new().with_state(|s| s.canon = Some(head));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    assert_eq!(up_to_date(&push(&e, &hub, None).unwrap()), Some(head));

    // a version named on the head after the hub took it
    let held = closure(&e, head, None);
    let tag: Hash = rdy_engine::engine::with_writer_result(&e.engine, |r| {
        let v = r.tag(&founder(), "v1.0.0", None, None)?;
        Ok(v.tag.parse().unwrap())
    })
    .unwrap();
    let tag_key = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    let late = closure(&e, head, Some(head));
    assert!(late.contains(&tag_key), "{late:?}");
    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    let (id, base, pushed, tip, resumed, sent) = taken(&out);
    assert_eq!(
        (base, pushed, tip, resumed),
        (Some(head), head, head, false)
    );
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    let want: BTreeSet<ObjectKey> = late.difference(&held).copied().collect();
    assert!(want.contains(&tag_key));
    assert_eq!(sent_keys, want);
    assert_eq!(sent.records, want.len() as u64);
    assert!(!sent_keys.contains(&landing(head)));
    // the batch's root is the head, named as a prerequisite
    let roots = hub.roots.lock().unwrap().clone();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].0, head);
    assert!(roots[0].1.contains(&landing(head)), "{:?}", roots[0].1);
    // the write expects the hub tip, the head, and carries the tag the hub lacked
    let w = &hub.writes()[0];
    assert_eq!(
        (w.push.as_str(), w.expected),
        (id.as_str(), Some(landing(head)))
    );
    assert_eq!(
        w.tags,
        [WriteTag {
            name: "v1.0.0".into(),
            disagreements: vec![]
        }]
    );
    assert_eq!(upload_of(&e, "shared"), None);

    // the hub took the tag with the write: nothing travels
    hub.clear();
    assert_eq!(up_to_date(&push(&e, &hub, None).unwrap()), Some(head));
    assert_eq!(hub.steps(), ["discovery", "state", "known"]);
    assert_eq!(upload_of(&e, "shared"), None);
}

/// D-H0BZN "Resume a put": a put stopped mid-way leaves the upload, and the next push resumes
/// it under the same push id; known names only the keys the quarantine lacks, and only those
/// travel. A write that was certainly not sent gives the upload back. After the quarantine
/// expires, every key is missing and all travel again.
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

    // resume: the same push id, known again, only what the quarantine lacks; the write never
    // leaves, so the upload comes back for the next push
    hub.clear();
    hub.plan_write(1, WritePlan::NotSent(transport_failure()));
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, E_UPSTREAM_TRANSPORT);
    let calls = hub.calls();
    assert_eq!(calls[2].step, "known");
    assert!(
        calls
            .iter()
            .filter(|c| matches!(c.step, "known" | "put" | "write"))
            .all(|c| c.push.as_deref() == Some(first.push.as_str()))
    );
    let resent: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert!(resent.is_disjoint(&landed), "a checked key travels again");
    let union: BTreeSet<ObjectKey> = resent.union(&landed).copied().collect();
    assert_eq!(union, all);
    assert_eq!(upload_of(&e, "shared"), Some(first.clone()));
    assert_eq!(pending_of(&e, "shared"), None);

    // the quarantine expired: every key is missing and all travel again, same push id
    hub.quarantine.lock().unwrap().clear();
    hub.bytes.lock().unwrap().clear();
    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    let (id, _, _, _, resumed, sent) = taken(&out);
    assert_eq!((id.as_str(), resumed), (first.push.as_str(), true));
    let again: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(again, all);
    assert_eq!(sent.records, all.len() as u64);
    assert!(sent.batches > 1, "{sent:?}");
    assert_eq!(upload_of(&e, "shared"), None);
}

/// D-H0BZN, D-3RSCW: a second push on a remote while one runs is `E_LOCKED` with the exact
/// words and sends nothing; two remotes do not block each other; a push never clears another
/// push's `upload` or `pending`; a found `pending` is looked up before known or put, and an
/// unknown outcome inside the quarantine life keeps both refs.
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
            "put",
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
    taken(&elsewhere.unwrap());
    // once the first push ends, the remote is free again
    push(&e, &hub, None).unwrap();

    // another push's upload: resumed, never cleared, even when the push fails before a put
    let e = env(1);
    let repo = repo_of(&e);
    let theirs = SyncUpload {
        repo_id: repo,
        url: e.url.clone(),
        push: P1.into(),
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
    // an upload of another hub repo: never resumed or replaced
    let e = env(1);
    let foreign = SyncUpload {
        url: format!("{}/acme/other", install()),
        repo_id: repo_of(&e),
        ..theirs.clone()
    };
    seed_sync(&e, "shared", vec![SyncStep::Upload(foreign.clone())]);
    let hub = Hub::new();
    assert_eq!(err(push(&e, &hub, None)).code, E_LOCKED);
    assert_eq!(upload_of(&e, "shared"), Some(foreign));
    assert!(!hub.steps().contains(&"known"));

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

    // a found pending write: looked up first; unknown inside the quarantine life keeps both refs
    let e = env(1);
    let head = e.landings[0];
    let sent = seed_pending(&e, P1, head, T0);
    let mine = SyncUpload {
        repo_id: repo_of(&e),
        url: e.url.clone(),
        push: P2.into(),
        kind: KIND_CANON,
        view: None,
        at_ms: T0,
    };
    seed_sync(&e, "shared", vec![SyncStep::Upload(mine.clone())]);
    let hub = Hub::new();
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, E_UPSTREAM_UNCERTAIN);
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert_eq!(hub.calls()[2].push.as_deref(), Some(P1));
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    assert_eq!(upload_of(&e, "shared"), Some(mine));
}

/// D-47HVF, D-0SC9C: hub refusals keep publish's mapping on every step. The hub's codes pass on
/// the hub's message and fix with its request id; one at another status is a protocol failure;
/// an unknown code, `E_HUB_SHARE_OFF` among them, is `E_UPSTREAM_REFUSED` with core's text.
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
        zstd: false,
    };
    let request = SyncStateRequest::new(&claim);
    let body = |code: &str, extra: &str| {
        format!(
            r#"{{"code":"{code}","message":"the hub's words","fix":"the hub's fix","issues":[],"request_id":"req_0042"{extra}}}"#
        )
    };
    let keys = [ObjectKey {
        kind: 1,
        hash: Hash::of(b"k"),
    }];
    let push_id = P1;
    let e = env(1);
    let inputs = rdy_engine::render_inputs::capture(&e.engine.read()).unwrap();
    let write = SyncWrite {
        expected: None,
        gate: Some(GateSettings {
            coverage_gate: "off".into(),
            coverage_target: 0,
            decisions_home: None,
            pack_digest: pack_digest(&inputs.pack),
            propose: ProposeGate {
                cadence: "break".into(),
                changes: 1,
            },
        }),
        kind: WriteKind::Landings,
        push: push_id.into(),
        render_inputs: Some(inputs),
        tags: vec![],
        view: None,
    };
    let root = landing(Hash::of(b"root"));
    let pull = KnownPull {
        after: None,
        base: None,
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let get = GetRequest {
        keys: keys.to_vec(),
        purpose: StreamPurpose::CanonHistory,
        root,
    };
    let mut served = |reply: Reply, step: &str| -> RdyError {
        s.reply(reply);
        s.accept_n(1);
        match step {
            "state" => err(wire.state(&cx, &request)),
            "known" => err(wire.known(&cx, push_id, &keys)),
            "known_pull" => err(wire.known_pull(&cx, &pull)),
            "get" => err(wire.get(&cx, &get, &mut Vec::new(), 1 << 20)),
            "outcome" => err(wire.outcome(&cx, push_id)),
            "write" => match wire.write(&cx, &write) {
                Err(WriteFailure::Refused(e)) => *e,
                other => panic!("{other:?}"),
            },
            _ => err(wire.put(&cx, push_id, &mut &b"batch"[..], 5)),
        }
    };
    for (code, status, step, extra) in [
        ("E_HUB_CHAIN_INVALID", 422, "put", ""),
        ("E_HUB_BEHIND", 409, "state", ""),
        ("E_HUB_VIEW_EXISTS", 409, "known", ""),
        // v2: the write, the outcome lookup and the pull steps
        ("E_HUB_BEHIND", 409, "write", ""),
        ("E_HUB_VALIDATION", 422, "write", ""),
        ("E_BAD_INPUT", 422, "write", ""),
        ("E_HUB_RDY_TOO_OLD", 422, "state", ""),
        ("E_HUB_NOT_FOUND", 404, "outcome", ""),
        ("E_HUB_BUSY", 503, "outcome", r#","retry_after":1"#),
        ("E_HUB_NOT_FOUND", 404, "known_pull", ""),
        ("E_HUB_NOT_FOUND", 404, "get", ""),
    ] {
        let x = served(Reply::json(status, &body(code, extra)), step);
        assert_eq!(
            (x.code, x.message.as_str(), x.fix.as_str()),
            (code, "the hub's words", "the hub's fix"),
            "{code} {step}"
        );
        assert!(
            x.issues.iter().any(|i| i.message == "hub request req_0042"),
            "{code} {step}: {:?}",
            x.issues
        );
    }
    // a move code at another status: a protocol failure (a put's is uncertain)
    let x = served(Reply::json(422, &body("E_HUB_BEHIND", "")), "state");
    assert_eq!(
        (x.code, x.message.as_str()),
        (E_UPSTREAM_PROTOCOL, UPSTREAM_PROTOCOL_MESSAGE)
    );
    let x = served(Reply::json(409, &body("E_HUB_CHAIN_INVALID", "")), "put");
    assert_eq!(x.code, E_UPSTREAM_UNCERTAIN);
    // a code in neither table: core's text, on a read and on the write
    let core_text = (
        E_UPSTREAM_REFUSED,
        UPSTREAM_REFUSED_MESSAGE,
        UPSTREAM_REFUSED_FIX,
    );
    let x = served(Reply::json(409, &body("E_HUB_SOMETHING_NEW", "")), "known");
    assert_eq!((x.code, x.message.as_str(), x.fix.as_str()), core_text);
    let share_off = r#"{"code":"E_HUB_SHARE_OFF","message":"this install does not offer Share yet","fix":"wait until this install offers Share","issues":[],"request_id":"req_0043"}"#;
    let x = served(Reply::json(403, share_off), "write");
    assert_eq!((x.code, x.message.as_str(), x.fix.as_str()), core_text);
    // the requests carried the sync media type
    let log = s.requests();
    assert!(
        log.iter()
            .filter(|r| r.method == "POST" && !r.target.contains("/objects/put"))
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

/// D-5Y8Y1: every step, get, write and the outcome lookup among them, uses the machine's
/// sign-in credential and the install's `ca_file`; no other credential call is made (no static
/// token, root-key pin or key-document fetch), and no credential reaches a sync state value.
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
    // a held repo id keeps the known step, so every step is checked
    let repo = repo_of(&e);
    let hub = Hub::new().with_state(|s| {
        s.batch_records = 2;
        s.repo_id = Some(repo);
    });
    // a lost write reply leaves a pending write
    hub.plan_write(1, WritePlan::Lost { apply: true });
    let s = Sync::new(&e, &hub);
    let x = err(s.push(PushRequest::default()));
    assert_eq!(x.code, E_UPSTREAM_UNCERTAIN);
    let pushed = pending_of(&e, "shared").unwrap();
    // the hub lands one more; the pull looks the write up, then gets the new landing
    let (_dir, hub_store) = copy_project(&e);
    let later = land_in(&hub_store, "later");
    hub.serve(hub_store.read());
    {
        let mut st = hub.state.lock().unwrap();
        st.canon = Some(later);
        // one get: the chunks of one fetch must share one header (see the report's seams)
        st.batch_records = 100_000;
    }
    hub.answer(
        &pushed.push,
        LookupOutcome::Taken {
            tip: landing(pushed.head),
        },
    );
    s.pull(PullRequest::default()).unwrap();
    let calls = hub.calls();
    let steps: BTreeSet<&str> = calls.iter().map(|c| c.step).collect();
    for step in ["get", "write", "outcome", "known_pull", "put", "known"] {
        assert!(steps.contains(step), "{step}: {steps:?}");
    }
    for c in &calls {
        assert_eq!(c.bearer, access(), "{}", c.step);
        assert_eq!(c.ca_file.as_deref(), Some(ca.as_path()), "{}", c.step);
    }
    // discovery has no sync base yet; every later step goes to the discovered one
    for c in &calls {
        let want = (c.step != "discovery").then(|| format!("{}/sync{REPO_PATH}", install()));
        assert_eq!(c.sync_base, want, "{}", c.step);
    }
    assert!(s.api.seen.lock().unwrap().is_empty());
    // the sync refs hold no token: pending (while it stood), upload, held and canon
    let text = format!("{pushed:?}");
    assert!(!text.contains("rdyh_"), "{text}");
    let read = e.engine.read();
    for (name, value) in read.reader().refs_with_prefix("meta/sync/").unwrap() {
        let v = String::from_utf8_lossy(&value);
        assert!(!v.contains("rdyh_"), "{name}");
        assert!(!v.contains(&access()), "{name}");
    }
    assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(later));
}

/// D-H0BZN "One push id per write": canon's write first, then each named view in Plan A's push
/// order, each after the previous write's reply. A view write names its view, expects the hub's
/// head (none for a new view) and sends no gate settings; its taken head becomes applied.
#[test]
fn scc8_push_sends_canon_before_selected_views() {
    let e = env(1);
    write_in(&e.engine, "beta", "b1");
    write_in(&e.engine, "alpha", "a1");
    let hub = Hub::new();
    let s = Sync::new(&e, &hub);
    let out = s
        .push(PushRequest {
            views: names(&["beta", "alpha"]),
            ..PushRequest::default()
        })
        .unwrap();
    taken(&out);
    let order: Vec<&str> = out.views.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(order, ["beta", "alpha"]);
    assert_eq!(
        hub.states.lock().unwrap()[0].views,
        Some(names(&["beta", "alpha"]))
    );
    assert_eq!(
        write_kinds(&hub),
        [
            (WriteKind::Landings, None),
            (WriteKind::View, Some("beta".into())),
            (WriteKind::View, Some("alpha".into())),
        ]
    );
    // each upload's known, put and write come after the previous write; the empty hub's canon
    // seed skips known
    assert_eq!(
        hub.steps(),
        [
            "discovery",
            "state",
            "put",
            "write",
            "known",
            "put",
            "write",
            "known",
            "put",
            "write"
        ]
    );
    for w in &hub.writes()[1..] {
        assert_eq!((w.expected, &w.gate), (None, &None));
    }
    for (name, end) in &out.views {
        let WriteEnd::Taken { tip, head, .. } = end else {
            panic!("{end:?}")
        };
        assert_eq!((*tip, *head), (view_head(&e, name), view_head(&e, name)));
        let v = e
            .engine
            .read()
            .reader()
            .sync_view("shared", name)
            .unwrap()
            .unwrap();
        assert_eq!(v.applied, Some(view_head(&e, name)));
        let root = e
            .engine
            .read()
            .reader()
            .view_chain_root(name, &view_head(&e, name))
            .unwrap();
        assert_eq!(v.root, root);
    }
    assert_eq!(upload_of(&e, "shared"), None);
    assert_eq!(pending_of(&e, "shared"), None);
}

/// D-H0BZN "Write reply": a refused write (`E_HUB_BEHIND`) clears `pending` and nothing else;
/// the hub's words pass on, and canon, `applied` and the landings stay as they were.
#[test]
fn scc8_write_compare_refusal_preserves_pending_work() {
    let e = env(2);
    let tip = e.landings[0];
    let applied = SyncCanon {
        repo_id: repo_of(&e),
        url: e.url.clone(),
        fetched: None,
        applied: Some(tip),
        held: None,
    };
    seed_sync(&e, "shared", vec![SyncStep::Canon(applied.clone())]);
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(1, WritePlan::Refuse(hub_words("E_HUB_BEHIND")));
    let head = e.engine.read().canon_head();
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Refused {
        push: Some(id),
        error,
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    assert_eq!(id, &hub.writes()[0].push);
    assert_eq!(
        (error.code, error.message.as_str(), error.fix.as_str()),
        ("E_HUB_BEHIND", "the hub's words", "the hub's fix")
    );
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(upload_of(&e, "shared"), None);
    assert_eq!(canon_of(&e, "shared"), Some(applied));
    assert_eq!(e.engine.read().canon_head(), head);
    assert_eq!(hub.state.lock().unwrap().canon, Some(tip));
}

/// D-H0BZN "Lost reply", "Resume a put": after a resumed put, a write whose reply is lost keeps
/// `pending` and stops with the sync words; the next push looks the outcome up first and never
/// sends that write again.
#[test]
fn scc8_resume_reuses_checked_objects_without_repeating_uncertain_write() {
    let e = env(3);
    let head = e.landings[2];
    let hub = Hub::new().with_state(|s| s.batch_records = 3);
    hub.fail_at("put", 2, transport_failure());
    err(push(&e, &hub, None));
    let first = upload_of(&e, "shared").unwrap();
    let landed: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();

    // the resumed upload sends only what is missing; its write's reply is lost
    hub.clear();
    hub.plan_write(1, WritePlan::Lost { apply: true });
    let x = err(push(&e, &hub, None));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            &*format!("the sync write of push {} may have completed", first.push),
            &*format!(
                "run rdy push or rdy pull again: rdy finds the outcome of push {} before it writes again",
                first.push
            ),
        )
    );
    let resent: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert!(resent.is_disjoint(&landed));
    assert!(
        hub.calls()
            .iter()
            .filter(|c| c.push.is_some())
            .all(|c| c.push.as_deref() == Some(first.push.as_str()))
    );
    let p = pending_of(&e, "shared").expect("the write stays pending");
    assert_eq!((p.push.as_str(), p.head), (first.push.as_str(), head));
    assert_eq!(upload_of(&e, "shared"), None);

    // the next push asks for the outcome first and never sends the write again
    hub.answer(&first.push, LookupOutcome::Taken { tip: landing(head) });
    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert!(hub.writes().is_empty());
    let lookup = out.lookup.clone().unwrap();
    assert_eq!(
        (lookup.push.as_str(), lookup.fetched),
        (first.push.as_str(), false)
    );
    assert_eq!(up_to_date(&out), Some(head));
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(head));
}

/// D-QCE8M: a `held` reply records the held head and request and leaves `applied`; the landings
/// stay unpushed (hub canon does not move).
#[test]
fn scc8_held_request_does_not_advance_known_canon() {
    let e = env(2);
    let tip = e.landings[0];
    let head = e.landings[1];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 7,
            version: 1,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Held {
        push: held_push,
        head: held_head,
        request,
        ..
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    assert_eq!(
        (*held_head, *request),
        (
            head,
            HubRequest {
                number: 7,
                version: 1
            }
        )
    );
    // D-PGQZM: held keeps the push id of the pending ref that the held reply clears
    let held = SyncHeld {
        head,
        request: 7,
        push: held_push.clone(),
    };
    assert_eq!(hub.writes()[0].push, *held_push);
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!((c.applied, c.held), (None, Some(held.clone())));
    // it is durable: a reopened copy of the store reads the same held request
    let (_copy_dir, reopened) = copy_project(&e);
    assert_eq!(
        reopened
            .read()
            .reader()
            .sync_canon("shared")
            .unwrap()
            .unwrap()
            .held,
        Some(held)
    );
    assert_eq!(hub.state.lock().unwrap().canon, Some(tip));
    assert_eq!(e.engine.read().canon_head(), Some(head));
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(upload_of(&e, "shared"), None);
}

/// D-DZDB7: canon and each view have their own push id; every known, put and write names only
/// its own push, and each write takes only its own quarantine.
#[test]
fn scc8_each_write_owns_one_push_quarantine() {
    let e = env(1);
    write_in(&e.engine, "alpha", "a1");
    write_in(&e.engine, "beta", "b1");
    let hub = Hub::new();
    let out = Sync::new(&e, &hub)
        .push(PushRequest {
            views: names(&["alpha", "beta"]),
            ..PushRequest::default()
        })
        .unwrap();
    let ids: Vec<String> = hub.writes().iter().map(|w| w.push.clone()).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), 3);
    // each write's push is the push of the known and put steps just before it
    let mut current = None;
    for c in hub.calls() {
        match c.step {
            "known" | "put" if current.is_none() => current = c.push.clone(),
            "known" | "put" => assert_eq!(c.push, current, "{}", c.step),
            "write" => {
                assert_eq!(c.push, current);
                current = None;
            }
            _ => {}
        }
    }
    // every quarantine was taken by its own write
    assert!(hub.quarantine.lock().unwrap().is_empty());
    let mut pushes: Vec<&str> = vec![taken(&out).0.leak()];
    for (_, end) in &out.views {
        let WriteEnd::Taken { push, .. } = end else {
            panic!("{end:?}")
        };
        pushes.push(push);
    }
    assert_eq!(pushes, ids.iter().map(String::as_str).collect::<Vec<_>>());
}

/// D-H0BZN: a canon refusal is listed, and each named view's write still runs.
#[test]
fn scc8_canon_refusal_still_writes_named_views() {
    let e = env(1);
    write_in(&e.engine, "alpha", "a1");
    write_in(&e.engine, "beta", "b1");
    let hub = Hub::new();
    hub.plan_write(1, WritePlan::Refuse(hub_words("E_HUB_VALIDATION")));
    let out = Sync::new(&e, &hub)
        .push(PushRequest {
            views: names(&["alpha", "beta"]),
            ..PushRequest::default()
        })
        .unwrap();
    let WriteEnd::Refused { error, .. } = &out.canon else {
        panic!("{out:?}")
    };
    assert_eq!(error.code, "E_HUB_VALIDATION");
    assert_eq!(out.views.len(), 2);
    for (_, end) in &out.views {
        assert!(matches!(end, WriteEnd::Taken { .. }), "{end:?}");
    }
    assert_eq!(hub.writes().len(), 3);
    assert_eq!(canon_of(&e, "shared"), None);
}

/// D-QMZ92, D-8RGWW, D-0WWYR: one capture per push at the pushed head's scope; every canon and
/// view write, with and without tags, sends it with this client's `rdy`; canon writes send the
/// gate settings with the captured pack's digest; view writes send none.
#[test]
fn scc8_every_write_has_one_render_capture_and_canon_gate_digest() {
    let e = env(1);
    let head = e.landings[0];
    write_in(&e.engine, "alpha", "a1");
    let hub = Hub::new();
    let s = Sync::new(&e, &hub);
    s.push(PushRequest {
        views: names(&["alpha"]),
        ..PushRequest::default()
    })
    .unwrap();
    assert_eq!(*s.renderer.scopes.lock().unwrap(), [Scope::At(head)]);
    let writes = hub.writes();
    assert_eq!(writes.len(), 2);
    let rdy = rdy_engine::formats::formats().rdy;
    for w in &writes {
        assert_eq!(w.render_inputs, writes[0].render_inputs);
        assert_eq!(w.render_inputs.as_ref().unwrap().rdy, rdy);
        assert!(w.tags.is_empty());
    }
    let read = e.engine.read();
    let want = CheckInputs::of_state(&read);
    let gate = writes[0].gate.clone().unwrap();
    assert_eq!(
        gate,
        GateSettings {
            coverage_gate: want.coverage_gate.as_str().into(),
            coverage_target: want.coverage_target,
            decisions_home: want.decisions_home.clone(),
            pack_digest: pack_digest(&writes[0].render_inputs.as_ref().unwrap().pack),
            propose: ProposeGate {
                cadence: want.propose_cadence.as_str().into(),
                changes: want.propose_changes,
            },
        }
    );
    assert_eq!(gate.pack_digest, pack_digest(read.pack()));
    assert_eq!(writes[1].gate, None);
    // the wire form: every member the fixture's canon write has
    let body: serde_json::Value = serde_json::to_value(&writes[0]).unwrap();
    let members: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        members,
        BTreeSet::from([
            "expected",
            "gate",
            "kind",
            "push",
            "render_inputs",
            "tags",
            "view"
        ])
    );
    assert_eq!(body["kind"], "landings");

    // a write with a tag: the same one capture, with the tag's disagreements
    rdy_engine::engine::with_writer_result(&e.engine, |r| {
        r.tag(&founder(), "v1.0.0", None, None)?;
        Ok(())
    })
    .unwrap();
    s.renderer.scopes.lock().unwrap().clear();
    hub.clear();
    s.push(PushRequest::default()).unwrap();
    assert_eq!(*s.renderer.scopes.lock().unwrap(), [Scope::At(head)]);
    let w = &hub.writes()[0];
    assert_eq!(w.render_inputs.as_ref().unwrap().rdy, rdy);
    assert_eq!(w.tags.len(), 1);
    assert!(w.gate.is_some());
}

/// D-H0BZN "Write reply": `taken` sets `applied` to the reply's tip; `held` sets `held`; the
/// request's number and version and `min_rdy_raised` reach the outcome.
#[test]
fn scc8_write_reply_records_outcome_request_version_and_minimum() {
    let e = env(1);
    let head = e.landings[0];
    let hub = Hub::new();
    hub.plan_write(
        1,
        WritePlan::Taken {
            min_rdy_raised: true,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Taken {
        tip,
        min_rdy_raised,
        ..
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    assert_eq!((*tip, *min_rdy_raised), (head, true));
    assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(head));

    let e = env(1);
    let head = e.landings[0];
    let hub = Hub::new();
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 3,
            version: 2,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Held {
        push: held_push,
        request,
        min_rdy_raised,
        ..
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    assert_eq!(
        (*request, *min_rdy_raised),
        (
            HubRequest {
                number: 3,
                version: 2
            },
            false
        )
    );
    assert_eq!(
        canon_of(&e, "shared").unwrap().held,
        Some(SyncHeld {
            head,
            request: 3,
            push: held_push.clone(),
        })
    );
}

/// D-QCE8M, D-H0BZN "Held": at the held head a push sends no write and reports the open
/// request; once local canon moves past it, the longer chain goes as a landings write that
/// expects the hub tip, and the hub's new request version is reported.
#[test]
fn scc8_held_head_suppresses_write_and_longer_chain_versions_request() {
    let e = env(2);
    let tip = e.landings[0];
    let head = e.landings[1];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 7,
            version: 1,
        },
    );
    push(&e, &hub, None).unwrap();

    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::HeldOpen { head: at, request } = out.canon else {
        panic!("{out:?}")
    };
    assert_eq!((at, request), (head, 7));
    assert_eq!(hub.steps(), ["discovery", "state"]);

    let next = land_in(&e.engine, "d9");
    hub.clear();
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 7,
            version: 2,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Held {
        push: held_push,
        request,
        head: h,
        ..
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    assert_eq!(
        (*request, *h),
        (
            HubRequest {
                number: 7,
                version: 2
            },
            next
        )
    );
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.expected),
        (WriteKind::Landings, Some(landing(tip)))
    );
    // the held landings travel again with the longer chain
    assert!(hub.sent_keys().contains(&landing(head)));
    assert_eq!(
        canon_of(&e, "shared").unwrap().held,
        Some(SyncHeld {
            head: next,
            request: 7,
            push: held_push.clone(),
        })
    );
}

/// D-H0BZN "Held": `held` clears when `applied` reaches its head, by a taken write or by a
/// pull. `held` keeps the held push's id (D-PGQZM), and a pull's outcome lookup of that push
/// clears `held` when its `request_state` is `closed`: the request closed with no landing.
#[test]
fn scc8_pull_clears_held_only_on_applied_or_closed_request() {
    // a taken write whose tip reaches the held head
    let e = env(2);
    let tip = e.landings[0];
    let head = e.landings[1];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 7,
            version: 1,
        },
    );
    push(&e, &hub, None).unwrap();
    assert!(canon_of(&e, "shared").unwrap().held.is_some());
    let next = land_in(&e.engine, "d9");
    hub.clear();
    taken(&push(&e, &hub, None).unwrap());
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!((c.applied, c.held), (Some(next), None));

    // a pull: the hub landed the request, so applied reaches the held head
    let e = env(2);
    let tip = e.landings[0];
    let head2 = e.landings[1];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 5,
            version: 1,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Held {
        push: held_push, ..
    } = &out.canon
    else {
        panic!("{out:?}")
    };
    // a pull while hub canon is below the held head and the request is open keeps held
    hub.answer_held(held_push, 5, "open");
    let (_dir, hub_store) = copy_project(&e);
    hub.serve(hub_store.read());
    let s = Sync::new(&e, &hub);
    s.pull(PullRequest::default()).unwrap();
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!(
        (c.applied, c.held),
        (
            Some(tip),
            Some(SyncHeld {
                head: head2,
                request: 5,
                push: held_push.clone(),
            })
        )
    );
    hub.state.lock().unwrap().canon = Some(head2);
    s.pull(PullRequest::default()).unwrap();
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!((c.applied, c.held), (Some(head2), None));
    let _ = head;

    // D-PGQZM "Clear", D-H67EN: a pull with `held` set asks the outcome lookup for
    // `held.push` before any known or get. `open` and `landed` (hub canon still below the held
    // head) keep `held`; `closed` (the request closed with no landing) clears it; `unknown`
    // clears it after hub canon applies, when canon does not reach the held head. `applied`
    // stays at the hub tip.
    for (request_state, kept) in [
        (Some("open"), true),
        (Some("landed"), true),
        (None, false),
        (Some("closed"), false),
    ] {
        let e = env(2);
        let tip = e.landings[0];
        let head = e.landings[1];
        let hub = Hub::new().with_state(|s| s.canon = Some(tip));
        hub.plan_write(
            1,
            WritePlan::Held {
                number: 9,
                version: 1,
            },
        );
        let out = push(&e, &hub, None).unwrap();
        let WriteEnd::Held {
            push: held_push, ..
        } = &out.canon
        else {
            panic!("{out:?}")
        };
        if let Some(rs) = request_state {
            hub.answer_held(held_push, 9, rs);
        }
        let (_dir, hub_store) = copy_project(&e);
        hub.serve(hub_store.read());
        hub.clear();
        let pulled = Sync::new(&e, &hub).pull(PullRequest::default()).unwrap();
        assert!(pulled.warnings.is_empty());
        let steps = hub.steps();
        assert_eq!(
            steps[..3],
            ["discovery", "state", "outcome"],
            "{request_state:?}"
        );
        let lookups: Vec<Option<String>> = hub
            .calls()
            .into_iter()
            .filter(|c| c.step == "outcome")
            .map(|c| c.push)
            .collect();
        assert_eq!(lookups, [Some(held_push.clone())], "{request_state:?}");
        let c = canon_of(&e, "shared").unwrap();
        let want = kept.then(|| SyncHeld {
            head,
            request: 9,
            push: held_push.clone(),
        });
        assert_eq!((c.applied, c.held), (Some(tip), want), "{request_state:?}");

        // the next push: a kept request sends no write; a closed one sends the write again,
        // under a new push id
        hub.clear();
        let out = push(&e, &hub, None).unwrap();
        if kept {
            assert!(
                matches!(out.canon, WriteEnd::HeldOpen { request: 9, .. }),
                "{out:?}"
            );
            assert!(hub.writes().is_empty());
        } else {
            let (p, base, pushed, _, _, _) = taken(&out);
            assert_ne!(&p, held_push);
            assert_eq!((base, pushed), (Some(tip), head));
        }
    }

    // D-H67EN: a refused lookup keeps `held`, the pull goes on (fetch and replay) and warns
    // W_UPSTREAM_HELD_LOOKUP with the hub's code and fix, or `pull again later` for a code with
    // no fix; the next pull asks again
    for (refusal, fix) in [
        (
            RdyError::new(
                "E_HUB_NOT_FOUND",
                "push not found",
                "check the push id, and sign in with the account that opened push p",
            ),
            "check the push id, and sign in with the account that opened push p",
        ),
        (
            RdyError::new("E_HUB_BUSY", "the hub is busy", ""),
            "pull again later",
        ),
    ] {
        let e = env(2);
        let tip = e.landings[0];
        let head = e.landings[1];
        let hub = Hub::new().with_state(|s| s.canon = Some(tip));
        hub.plan_write(
            1,
            WritePlan::Held {
                number: 3,
                version: 1,
            },
        );
        let out = push(&e, &hub, None).unwrap();
        let WriteEnd::Held {
            push: held_push, ..
        } = &out.canon
        else {
            panic!("{out:?}")
        };
        let code = refusal.code;
        hub.fail_at("outcome", 1, refusal);
        let (_dir, hub_store) = copy_project(&e);
        hub.serve(hub_store.read());
        hub.clear();
        let s = Sync::new(&e, &hub);
        let pulled = s.pull(PullRequest::default()).unwrap();
        assert_eq!(
            pulled.warnings,
            [SyncWarning {
                code: "W_UPSTREAM_HELD_LOOKUP".into(),
                message: format!("the outcome of held push {held_push} could not be read: {code}"),
                fix: fix.into(),
            }]
        );
        assert!(pulled.canon.is_some(), "the pull went on: {pulled:?}");
        let steps = hub.steps();
        assert_eq!(steps[..3], ["discovery", "state", "outcome"]);
        assert!(steps.contains(&"known_pull"), "{steps:?}");
        let held = Some(SyncHeld {
            head,
            request: 3,
            push: held_push.clone(),
        });
        let c = canon_of(&e, "shared").unwrap();
        assert_eq!((c.applied, c.held.clone()), (Some(tip), held.clone()));
        // the next pull asks again
        hub.answer_held(held_push, 3, "open");
        hub.clear();
        let pulled = s.pull(PullRequest::default()).unwrap();
        assert!(pulled.warnings.is_empty());
        assert_eq!(hub.steps().iter().filter(|s| **s == "outcome").count(), 1);
        assert_eq!(canon_of(&e, "shared").unwrap().held, held);
    }

    // a pull with no held request asks no outcome lookup
    let e = env(1);
    let tip = e.landings[0];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    let (_dir, hub_store) = copy_project(&e);
    hub.serve(hub_store.read());
    Sync::new(&e, &hub).pull(PullRequest::default()).unwrap();
    assert!(!hub.steps().contains(&"outcome"));
}

/// D-H0BZN "Lost reply", "Outcome lookup": a found `pending` is looked up before any known,
/// put, get or write, on push and on pull. Taken and held apply the reply rules; refused (any
/// code, `E_HUB_BUSY` from the hub's repair among them) clears it and the push goes on with a
/// new push id. F7: a lookup the hub refuses keeps `pending` and sends nothing else.
#[test]
fn scc8_pending_lookup_precedes_put_or_write_and_never_resends() {
    // taken
    let e = env(1);
    let head = e.landings[0];
    seed_pending(&e, P1, head, T0);
    let hub = Hub::new().with_state(|s| s.canon = Some(head));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    hub.answer(P1, LookupOutcome::Taken { tip: landing(head) });
    let out = push(&e, &hub, None).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert!(matches!(
        out.lookup.unwrap().outcome,
        LookupOutcome::Taken { .. }
    ));
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(head));

    // held: recorded; at the held head no write follows
    let e = env(1);
    let head = e.landings[0];
    seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.answer(
        P1,
        LookupOutcome::Held {
            request: HubRequest {
                number: 4,
                version: 1,
            },
            tip: None,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert!(matches!(out.canon, WriteEnd::HeldOpen { request: 4, .. }));
    assert_eq!(
        canon_of(&e, "shared").unwrap().held,
        Some(SyncHeld {
            head,
            request: 4,
            push: P1.into(),
        })
    );

    // refused with the repair's code: cleared, and the push goes on under a new push id
    let e = env(1);
    let head = e.landings[0];
    seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.answer(
        P1,
        LookupOutcome::Refused {
            code: "E_HUB_BUSY".into(),
        },
    );
    let out = push(&e, &hub, None).unwrap();
    assert_eq!(
        hub.steps(),
        ["discovery", "state", "outcome", "put", "write"]
    );
    let (id, ..) = taken(&out);
    assert_ne!(id, P1);
    assert_eq!(pending_of(&e, "shared"), None);

    // F7: the lookup itself refused (the write intent is open): the hub's busy refusal is waited
    // out to the polling bound (r59016), then pending stays and nothing else goes
    let e = env(1);
    let head = e.landings[0];
    let sent = seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    script_busy(&hub, 1, 200);
    let x = err(push(&e, &hub, None));
    assert_sync_uncertain(&x, P1);
    assert_eq!(asks(&hub).len(), 82);
    assert_eq!(hub.steps().len(), 2 + 82);
    assert!(hub.writes().is_empty());
    assert_eq!(pending_of(&e, "shared"), Some(sent.clone()));
    // E_HUB_NOT_FOUND (Q5): kept too
    let hub = Hub::new();
    hub.fail_at("outcome", 1, hub_words("E_HUB_NOT_FOUND"));
    assert_eq!(err(push(&e, &hub, None)).code, "E_HUB_NOT_FOUND");
    assert_eq!(pending_of(&e, "shared"), Some(sent.clone()));

    // on pull: the lookup comes first too
    let hub = Hub::new();
    script_busy(&hub, 1, 200);
    let x = err(Sync::new(&e, &hub).pull(PullRequest::default()));
    assert_sync_uncertain(&x, P1);
    assert_eq!(asks(&hub).len(), 82);
    assert_eq!(hub.steps().len(), 2 + 82);
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    let hub = Hub::new();
    hub.answer(
        P1,
        LookupOutcome::Refused {
            code: "E_HUB_BEHIND".into(),
        },
    );
    let out = Sync::new(&e, &hub).pull(PullRequest::default()).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert!(out.lookup.is_some());
    assert_eq!(pending_of(&e, "shared"), None);
}

/// D-H0BZN: `unknown` while less than `quarantine_seconds` has passed since `pending.at_ms`
/// stops with the sync words and keeps `pending`; after it, the write did not take: `pending`
/// clears and the push goes on.
#[test]
fn scc8_unknown_outcome_waits_for_quarantine_expiry() {
    let e = env(1);
    let head = e.landings[0];
    let sent = seed_pending(&e, P1, head, T0);
    let hub = Hub::new().with_state(|s| s.quarantine_seconds = 60);
    let s = Sync::new(&e, &hub);
    s.clock.0.store(T0 + 59_999, Ordering::SeqCst);
    let x = err(s.push(PushRequest::default()));
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            &*format!("the sync write of push {P1} may have completed"),
            &*format!(
                "run rdy push or rdy pull again: rdy finds the outcome of push {P1} before it writes again"
            ),
        )
    );
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);

    hub.clear();
    s.clock.0.store(T0 + 60_000, Ordering::SeqCst);
    let out = s.push(PushRequest::default()).unwrap();
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(
        hub.steps(),
        ["discovery", "state", "outcome", "put", "write"]
    );
    assert_eq!(out.lookup.as_ref().unwrap().outcome, LookupOutcome::Unknown);
    assert_ne!(taken(&out).0, P1);
}

/// D-H0BZN "Outcome lookup": the pushed head in the store and at the hub, with no lookup answer,
/// changes nothing; a `taken` tip other than the pushed head runs the fetch batch first, then
/// records the tip as applied.
#[test]
fn scc8_outcome_lookup_never_infers_from_store_and_fetches_changed_head() {
    let e = env(2);
    let head = e.landings[1];
    let sent = seed_pending(&e, P1, head, T0);
    let hub = Hub::new().with_state(|s| s.canon = Some(head));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    script_busy(&hub, 1, 200);
    assert_sync_uncertain(&err(push(&e, &hub, None)), P1);
    assert_eq!(asks(&hub).len(), 82);
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    assert_eq!(canon_of(&e, "shared"), None);

    // the hub took the write and moved on: the lookup fetches the newer tip first
    let (_dir, hub_store) = copy_project(&e);
    let later = land_in(&hub_store, "later");
    hub.serve(hub_store.read());
    hub.state.lock().unwrap().canon = Some(later);
    hub.answer(
        P1,
        LookupOutcome::Taken {
            tip: landing(later),
        },
    );
    hub.clear();
    let out = Sync::new(&e, &hub).pull(PullRequest::default()).unwrap();
    let steps = hub.steps();
    assert_eq!(
        &steps[..5],
        ["discovery", "state", "outcome", "known_pull", "get"]
    );
    assert!(out.lookup.unwrap().fetched);
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!((c.applied, c.fetched), (Some(later), Some(later)));
    assert_eq!(e.engine.read().canon_head(), Some(later));
    assert_eq!(pending_of(&e, "shared"), None);
}

/// D-H0BZN "Uncertain text": both sync texts exactly, with `<push>` filled; publish keeps its
/// own text.
#[test]
fn scc8_uncertain_text_is_exact_and_publish_text_is_unchanged() {
    assert_eq!(
        SYNC_UNCERTAIN_MESSAGE,
        "the sync write of push <push> may have completed"
    );
    assert_eq!(
        SYNC_UNCERTAIN_FIX,
        "run rdy push or rdy pull again: rdy finds the outcome of push <push> before it writes again"
    );
    let x = sync_uncertain(P1);
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            &*format!("the sync write of push {P1} may have completed"),
            &*format!(
                "run rdy push or rdy pull again: rdy finds the outcome of push {P1} before it writes again"
            ),
        )
    );
    assert_eq!(
        UPSTREAM_UNCERTAIN_MESSAGE,
        "the hub request may have completed"
    );
    assert_eq!(
        UPSTREAM_UNCERTAIN_FIX,
        "check its state on the hub before trying again"
    );
    let p = upstream_uncertain();
    assert_eq!(
        (p.message.as_str(), p.fix.as_str()),
        (UPSTREAM_UNCERTAIN_MESSAGE, UPSTREAM_UNCERTAIN_FIX)
    );
}

/// D-H0BZN "Force": a forced push sends kind `force` expecting the hub tip only when local canon
/// does not hold it; else it is a plain push and says force was not needed. The preview names
/// the landings that leave canon, with no request and no write.
#[test]
fn scc8_force_push_moves_hub_canon_and_previews_leaving() {
    let e = env(2);
    let head = e.landings[1];
    // the hub's own landing after the first: a chain local canon does not hold
    let (_dir, hub_store) = copy_project(&e);
    let theirs = land_in(&hub_store, "theirs");
    let hub = Hub::new().with_state(|s| s.canon = Some(theirs));
    assert_eq!(err(push(&e, &hub, None)).code, E_DIVERGED);

    // the preview reads this store only: the hub tip is not here, so the landings that leave
    // are unknown and the preview is not complete (it never certifies an empty list)
    let read = e.engine.read();
    let p = canon_preview(&read, "shared", Some(theirs), PreviewKind::ForcePush).unwrap();
    assert_eq!(
        p,
        CanonPreview {
            remote: "shared".into(),
            before: Some(theirs),
            after: Some(head),
            leaving: vec![],
            complete: false,
        }
    );
    // diverged chains: the hub's landing is not here, so nothing certifies what leaves
    let d = env(2);
    let (_d_dir, d_hub_store) = copy_project(&d);
    let hub_side = land_in(&d_hub_store, "theirs");
    let mine = land_in(&d.engine, "mine");
    let p = canon_preview(
        &d.engine.read(),
        "shared",
        Some(hub_side),
        PreviewKind::ForcePush,
    )
    .unwrap();
    assert_eq!(
        (p.before, p.after, p.leaving.clone(), p.complete),
        (Some(hub_side), Some(mine), vec![], false)
    );
    // once a pull brings the hub's history here, the preview is complete: a hub tip whose chain
    // this store holds names each landing that leaves
    let d_hub = Hub::new().with_state(|s| s.canon = Some(hub_side));
    d_hub.serve(d_hub_store.read());
    Sync::new(&d, &d_hub).pull(PullRequest::default()).unwrap();
    let d_read = d.engine.read();
    assert_ne!(d_read.canon_head(), Some(mine));
    let p = canon_preview(&d_read, "shared", Some(hub_side), PreviewKind::ForcePush).unwrap();
    assert_eq!((p.leaving.clone(), p.complete), (vec![], true));
    let p = canon_preview(&d_read, "shared", Some(mine), PreviewKind::ForcePush).unwrap();
    assert_eq!((p.leaving.clone(), p.complete), (vec![mine], true));
    drop(d_read);
    let take = canon_preview(&read, "shared", Some(e.landings[0]), PreviewKind::TakePull).unwrap();
    assert_eq!(take.leaving, [head]);
    drop(read);
    let writes_before = hub.writes().len();
    let s = Sync::new(&e, &hub);
    let p = s
        .service()
        .preview(&e.engine, &agent(), None, PreviewKind::ForcePush)
        .unwrap();
    assert_eq!(p.before, Some(theirs));
    assert_eq!(hub.writes().len(), writes_before);
    assert_eq!(pending_of(&e, "shared"), None);

    hub.clear();
    let out = s
        .push(PushRequest {
            force: true,
            ..PushRequest::default()
        })
        .unwrap();
    assert!(!out.force_not_needed);
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.expected),
        (WriteKind::Force, Some(landing(theirs)))
    );
    assert_eq!(serde_json::to_value(w).unwrap()["kind"], "force");
    assert_eq!(taken(&out).3, head);

    // local canon holds the hub tip: a plain push, and force was not needed
    let e = env(2);
    let hub = Hub::new().with_state(|s| s.canon = Some(e.landings[0]));
    let out = Sync::new(&e, &hub)
        .push(PushRequest {
            force: true,
            ..PushRequest::default()
        })
        .unwrap();
    assert!(out.force_not_needed);
    assert_eq!(hub.writes()[0].kind, WriteKind::Landings);
}

/// D-KE83X, D-H0BZN (final review F1): a push that moves canon or a view still names the root it
/// moves to when the hub already holds every object. Known names nothing missing, so one batch
/// of no record names the head as its prerequisite before the write, under the write's push id;
/// the upload is recorded before that put, so a lost put reply leaves it to resume and a put
/// that certainly failed clears it. An ordinary push, a forced push and a view push alike.
#[test]
fn scc8_moving_push_with_every_object_held_names_its_root() {
    // ordinary: the hub tip is the first landing and the hub holds the second's whole closure
    let e = env(2);
    let (first, head) = (e.landings[0], e.landings[1]);
    let hub = Hub::new().with_state(|s| s.canon = Some(first));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    let out = push(&e, &hub, None).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);
    let (id, base, pushed, tip, resumed, sent) = taken(&out);
    assert_eq!(
        (base, pushed, tip, resumed),
        (Some(first), head, head, false)
    );
    assert_eq!(
        sent,
        HistorySentView {
            batches: 1,
            records: 0
        }
    );
    assert_eq!(*hub.puts.lock().unwrap(), [(id.clone(), vec![])]);
    assert_eq!(*hub.roots.lock().unwrap(), [(head, vec![landing(head)])]);
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.push.as_str(), w.expected),
        (WriteKind::Landings, id.as_str(), Some(landing(first)))
    );
    assert_eq!(upload_of(&e, "shared"), None);
    assert_eq!(pending_of(&e, "shared"), None);

    // the upload is recorded before the root batch: a lost put reply leaves it to resume
    let e = env(2);
    let (first, head) = (e.landings[0], e.landings[1]);
    let hub = Hub::new().with_state(|s| s.canon = Some(first));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    hub.fail_at(
        "put",
        1,
        RdyError::new(
            E_UPSTREAM_UNCERTAIN,
            UPSTREAM_UNCERTAIN_MESSAGE,
            UPSTREAM_UNCERTAIN_FIX,
        ),
    );
    assert_eq!(err(push(&e, &hub, None)).code, E_UPSTREAM_UNCERTAIN);
    let up = upload_of(&e, "shared").expect("the upload stays to resume");
    assert_eq!(hub.calls().last().unwrap().push.as_deref(), Some(&*up.push));
    assert!(hub.writes().is_empty());
    // the resumed push sends the root batch under the same push id, then writes
    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    let (id, _, _, _, resumed, _) = taken(&out);
    assert_eq!((id.as_str(), resumed), (up.push.as_str(), true));
    assert_eq!(*hub.puts.lock().unwrap(), [(id.clone(), vec![])]);
    assert_eq!(upload_of(&e, "shared"), None);
    // a root batch that certainly failed: its own upload is cleared, and no write is sent
    let e = env(2);
    let (first, head) = (e.landings[0], e.landings[1]);
    let hub = Hub::new().with_state(|s| s.canon = Some(first));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    hub.fail_at("put", 1, transport_failure());
    assert_eq!(err(push(&e, &hub, None)).code, E_UPSTREAM_TRANSPORT);
    assert_eq!(upload_of(&e, "shared"), None);
    assert!(hub.writes().is_empty());

    // forced: the hub tip is a landing local canon lacks; the hub holds local canon's closure
    let e = env(2);
    let head = e.landings[1];
    let (_dir, hub_store) = copy_project(&e);
    let theirs = land_in(&hub_store, "theirs");
    let hub = Hub::new().with_state(|s| s.canon = Some(theirs));
    *hub.held.lock().unwrap() = closure(&e, head, None);
    let out = Sync::new(&e, &hub)
        .push(PushRequest {
            force: true,
            ..PushRequest::default()
        })
        .unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);
    let (id, _, pushed, tip, _, _) = taken(&out);
    assert_eq!((pushed, tip), (head, head));
    assert_eq!(*hub.puts.lock().unwrap(), [(id.clone(), vec![])]);
    assert_eq!(*hub.roots.lock().unwrap(), [(head, vec![landing(head)])]);
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.push.as_str(), w.expected),
        (WriteKind::Force, id.as_str(), Some(landing(theirs)))
    );
    assert_eq!(hub.state.lock().unwrap().canon, Some(head));

    // a view: the hub holds every op of the local head, and its head is an earlier op
    let e = env(1);
    write_in(&e.engine, "v1", "a");
    let v1_first = view_head(&e, "v1");
    write_in(&e.engine, "v1", "b");
    let v1 = view_head(&e, "v1");
    let read = e.engine.read();
    let ops = rdy_store::closure::history_closure_before(
        read.reader(),
        StreamPurpose::ViewHistory,
        v1,
        &BTreeSet::new(),
        None,
        read.seq(),
    )
    .unwrap()
    .records;
    let canon = e.landings[0];
    let hub = Hub::new().with_state(|s| s.canon = Some(canon));
    {
        let mut held = hub.held.lock().unwrap();
        *held = closure(&e, canon, None);
        held.extend(ops);
    }
    hub.views
        .lock()
        .unwrap()
        .insert("v1".into(), (view_root(&read, v1), v1_first));
    drop(read);
    let out = Sync::new(&e, &hub)
        .push(PushRequest {
            views: names(&["v1"]),
            ..PushRequest::default()
        })
        .unwrap();
    assert_eq!(up_to_date(&out), Some(canon));
    let op = |hash| ObjectKey {
        kind: kinds::VIEW_OP,
        hash,
    };
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.view.as_deref(), w.expected),
        (WriteKind::View, Some("v1"), Some(op(v1_first)))
    );
    assert_eq!(*hub.puts.lock().unwrap(), [(w.push.clone(), vec![])]);
    assert_eq!(*hub.roots.lock().unwrap(), [(v1, vec![op(v1)])]);
    match &out.views[..] {
        [(name, WriteEnd::Taken { tip, .. })] => assert_eq!((name.as_str(), *tip), ("v1", v1)),
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// Restore through the exchange (Task 8b: D-J3YA0, D-D5J18, D-QMZ92, D-0WWYR, D-H0BZN, D-47HVF)
// ---------------------------------------------------------------------------------------------

/// The fake hub behind a tap: each put's batch bytes are kept, and with `taken_on_lookup` the
/// outcome lookup answers taken at the hub's tip (the write took, its reply was lost).
struct Tap<'a> {
    hub: &'a Hub,
    batches: Mutex<Vec<Vec<u8>>>,
    taken_on_lookup: bool,
}

impl<'a> Tap<'a> {
    fn new(hub: &'a Hub, taken_on_lookup: bool) -> Tap<'a> {
        Tap {
            hub,
            batches: Mutex::default(),
            taken_on_lookup,
        }
    }
}

impl SyncWire for Tap<'_> {
    fn discovery(
        &self,
        cx: &UploadContext,
        remote: &str,
    ) -> Result<rdy_engine::hub_publish_wire::Discovery, RdyError> {
        self.hub.discovery(cx, remote)
    }

    fn state(
        &self,
        cx: &UploadContext,
        request: &SyncStateRequest,
    ) -> Result<rdy_engine::hub_sync_wire::SyncState, RdyError> {
        self.hub.state(cx, request)
    }

    fn known(
        &self,
        cx: &UploadContext,
        push: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        self.hub.known(cx, push, keys)
    }

    fn put(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut dyn std::io::Read,
        len: u64,
    ) -> Result<rdy_engine::hub_publish_wire::ObjectPut, RdyError> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        self.batches.lock().unwrap().push(bytes.clone());
        self.hub.put(cx, push, &mut &bytes[..], len)
    }

    fn known_pull(
        &self,
        cx: &UploadContext,
        request: &KnownPull,
    ) -> Result<rdy_engine::hub_sync_wire::KnownPage, RdyError> {
        self.hub.known_pull(cx, request)
    }

    fn get(
        &self,
        cx: &UploadContext,
        request: &GetRequest,
        sink: &mut dyn std::io::Write,
        limit: u64,
    ) -> Result<u64, RdyError> {
        self.hub.get(cx, request, sink, limit)
    }

    fn write(
        &self,
        cx: &UploadContext,
        request: &SyncWrite,
    ) -> Result<rdy_engine::hub_sync_wire::WriteReply, WriteFailure> {
        self.hub.write(cx, request)
    }

    fn outcome(
        &self,
        cx: &UploadContext,
        push: &str,
    ) -> Result<rdy_engine::hub_sync_wire::OutcomeReply, RdyError> {
        if !self.taken_on_lookup {
            return self.hub.outcome(cx, push);
        }
        self.hub.enter("outcome", cx, Some(push), None)?;
        let tip = self.hub.state.lock().unwrap().canon.expect("a hub tip");
        Ok(rdy_engine::hub_sync_wire::OutcomeReply {
            outcome: LookupOutcome::Taken { tip: landing(tip) },
            kind: Some("restore".into()),
            view: None,
            request_state: None,
        })
    }
}

/// The restore of `dump` from the project `e` (its share remote, user config and sign-in) over
/// `wire`, with its temporary store made under `scratch`.
fn restore_with(
    e: &Env,
    wire: &dyn SyncWire,
    renderer: &Renderer,
    scratch: &std::path::Path,
    dump: &std::path::Path,
) -> Result<RestoreOutcome, RdyError> {
    restore_polled(e, wire, renderer, scratch, dump, &PollClock::default())
}

/// [`restore_with`] whose busy outcome lookups run on the fake clock `poll`.
fn restore_polled(
    e: &Env,
    wire: &dyn SyncWire,
    renderer: &Renderer,
    scratch: &std::path::Path,
    dump: &std::path::Path,
    poll: &PollClock,
) -> Result<RestoreOutcome, RdyError> {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    SyncService::new(&e.project, &e.user_file, &api, &clock, wire, renderer)
        .with_outcome_poll_clock(poll)
        .scratch_in(scratch)
        .restore(
            &agent(),
            RestoreRequest {
                dump: vec![dump.to_path_buf()],
                remote: None,
            },
        )
}

/// The read-only preview of the restore of `dump`, as [`restore_with`] runs it.
fn preview_with(
    e: &Env,
    wire: &dyn SyncWire,
    scratch: &std::path::Path,
    dump: &std::path::Path,
) -> Result<CanonPreview, RdyError> {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let renderer = Renderer::default();
    SyncService::new(&e.project, &e.user_file, &api, &clock, wire, &renderer)
        .scratch_in(scratch)
        .restore_preview(
            &agent(),
            RestoreRequest {
                dump: vec![dump.to_path_buf()],
                remote: None,
            },
        )
}

/// A full dump of `engine`'s project at `dir/name`.
fn dump_of(engine: &Local, dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let out = dir.join(name);
    engine.read().dump(&out).unwrap();
    out
}

const ORIGIN: rdy_store::segment::Location = rdy_store::segment::Location {
    segment: 0,
    offset: 0,
};

/// The sync frames of `read`'s log from `from`: what a push of them brings to a hub store.
fn frames(read: &ReadState, from: rdy_store::segment::Location) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    read.reader()
        .for_each_frame_from(from, read.seq(), |_, raw, _, payload, _| {
            if rdy_store::sync_stream::is_sync_kind(rdy_store::codec::logical_kind(raw)) {
                out.push((raw, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

/// The first op of the chain of view op `head`.
fn view_root(read: &ReadState, head: Hash) -> Hash {
    let mut at = head;
    while let Some(prev) = read.reader().view_op(&at).unwrap().prev {
        at = prev;
    }
    at
}

/// A held chain beside canon: its sync frames and its head.
type HeldChain = (Vec<(u8, Vec<u8>)>, Hash);

/// The dump at `dir/name` of a hub store (a holder of `e`'s repo, D-DZDB7) that holds `e`'s
/// canon, each view of `views` at its head in `e`, and, when given, the held chain `held`
/// (its frames and head) beside canon.
fn hub_dump(
    e: &Env,
    views: &[&str],
    held: Option<HeldChain>,
    dir: &std::path::Path,
    name: &str,
) -> std::path::PathBuf {
    use rdy_store::hub_canon::{CanonMove, Moved};
    let read = e.engine.read();
    let head = read.canon_head().unwrap();
    let mut h = Rdy::create_holder(&dir.join(format!("{name}-holder")), repo_of(e)).unwrap();
    let s = h.store_mut();
    let b = s
        .reader()
        .check_move_batch(frames(&read, ORIGIN), head)
        .unwrap();
    assert_eq!(
        s.move_canon(&b, &CanonMove::Seed, None, T0).unwrap(),
        Moved::Moved { tip: head }
    );
    for v in views {
        let vh = read.view_head(v).unwrap().head;
        let b = s.reader().check_move_batch(vec![], vh).unwrap();
        let mv = CanonMove::SetViewHead {
            name: v.to_string(),
            root: view_root(&read, vh),
        };
        assert_eq!(
            s.move_canon(&b, &mv, None, T0).unwrap(),
            Moved::Moved { tip: vh }
        );
    }
    if let Some((records, hh)) = held {
        let b = s.reader().check_move_batch(records, hh).unwrap();
        assert_eq!(
            s.move_canon(&b, &CanonMove::Hold, Some(head), T0).unwrap(),
            Moved::Moved { tip: hh }
        );
    }
    // reopened, as a hub opens its store: the moves above went past the handle's published
    // state
    h.close().unwrap();
    let h = Rdy::open_holder(&dir.join(format!("{name}-holder"))).unwrap();
    let out = dir.join(name);
    h.read().dump(&out).unwrap();
    h.close().unwrap();
    out
}

/// Whether `dir` holds nothing.
fn is_empty_dir(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

fn append(file: &std::path::Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().append(true).open(file).unwrap();
    std::io::Write::write_all(&mut f, text.as_bytes()).unwrap();
}

fn tag_head(e: &Env, name: &str) {
    let name = name.to_string();
    rdy_engine::engine::with_writer_result(&e.engine, move |r| {
        r.tag(&founder(), &name, None, None)?;
        Ok(())
    })
    .unwrap();
}

#[track_caller]
fn taken_end(end: &Option<WriteEnd>) -> (Option<Hash>, Hash, Hash) {
    match end {
        Some(WriteEnd::Taken {
            base, head, tip, ..
        }) => (*base, *head, *tip),
        other => panic!("{other:?}"),
    }
}

/// D-J3YA0: into an empty hub repo a restore is one write of the kind `restore` expecting
/// nothing (the seed move); into a hub repo with canon it expects the state's tip (the hub
/// chooses force); a repo id mismatch stops before any known or put. A hub dump's views follow
/// the canon write's reply, each with its own push id, when the hub lacks them or the dump's
/// chain holds the hub's head; a hub view past the dump's is not sent and is listed. A project
/// dump's views stay home, and a held chain travels only as records that canon or a view names.
/// The preview names what the move removes and writes nothing.
#[test]
fn scc8_restore_seeds_or_forces_and_keeps_newer_hub_views() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(2);
    write_in(&src.engine, "draft", "x1");
    let head = src.landings[1];
    let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
    let renderer = Renderer::default();

    // into an empty hub repo: one write, kind restore, expecting nothing; the view stays home
    let hub = Hub::new();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    assert!(!out.holder);
    assert_eq!(out.remote, "shared");
    assert_eq!(write_kinds(&hub), [(WriteKind::Restore, None)]);
    let w = &hub.writes()[0];
    assert_eq!(w.expected, None);
    assert_eq!(serde_json::to_value(w).unwrap()["kind"], "restore");
    assert_eq!(taken_end(&out.canon), (None, head, head));
    assert!(out.views.is_empty() && out.kept.is_empty());
    assert_eq!(hub.states.lock().unwrap()[0].views, None);
    assert!(hub.sent_keys().iter().all(|k| k.kind != kinds::VIEW_OP));
    let empty = preview_with(&caller, &Hub::new(), scratch.path(), &dump).unwrap();
    assert_eq!(
        empty,
        CanonPreview {
            remote: "shared".into(),
            before: None,
            after: Some(head),
            leaving: vec![],
            complete: true,
        }
    );

    // a hub tip the dump holds: the write expects it, and the history goes above it
    let base = src.landings[0];
    let with_canon = || {
        Hub::new().with_state(|s| {
            s.repo_id = Some(repo_of(&src));
            s.canon = Some(base);
        })
    };
    let hub = with_canon();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    let w = &hub.writes()[0];
    assert_eq!(
        (w.kind, w.expected, w.view.clone()),
        (WriteKind::Restore, Some(landing(base)), None)
    );
    assert_eq!(taken_end(&out.canon), (Some(base), head, head));
    assert_eq!(
        preview_with(&caller, &with_canon(), scratch.path(), &dump).unwrap(),
        CanonPreview {
            remote: "shared".into(),
            before: Some(base),
            after: Some(head),
            leaving: vec![],
            complete: true,
        }
    );

    // a hub tip the dump lacks: seed or force is the hub's choice; the write expects that tip
    let other = Hash::of(b"a tip only the hub holds");
    let lacking = || {
        Hub::new().with_state(|s| {
            s.repo_id = Some(repo_of(&src));
            s.canon = Some(other);
        })
    };
    let hub = lacking();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    assert_eq!(
        (hub.writes()[0].kind, hub.writes()[0].expected),
        (WriteKind::Restore, Some(landing(other)))
    );
    assert_eq!(taken_end(&out.canon), (None, head, head));
    let p = preview_with(&caller, &lacking(), scratch.path(), &dump).unwrap();
    assert_eq!(
        (p.before, p.after, p.complete),
        (Some(other), Some(head), false)
    );
    assert_eq!(hub.writes().len(), 1);

    // another repo's hub: E_REPO_MISMATCH before any known or put
    let hub = Hub::new().with_state(|s| s.repo_id = Some(repo_of(&caller)));
    let e = err(restore_with(
        &caller,
        &hub,
        &renderer,
        scratch.path(),
        &dump,
    ));
    assert_eq!(e.code, E_REPO_MISMATCH);
    assert_eq!(hub.steps(), ["discovery", "state"]);

    // a hub dump: new and extending views follow canon; a hub view past the dump's stays
    let hsrc = env(1);
    write_in(&hsrc.engine, "v1", "a");
    write_in(&hsrc.engine, "v2", "b");
    let v2_first = view_head(&hsrc, "v2");
    write_in(&hsrc.engine, "v2", "c");
    write_in(&hsrc.engine, "v3", "d");
    let at = hsrc.engine.read().seq();
    let (_copy_dir, copy) = copy_project(&hsrc);
    let q = land_in(&copy, "held");
    let held = (frames(&copy.read(), at), q);
    let hdump = hub_dump(
        &hsrc,
        &["v1", "v2", "v3"],
        Some(held),
        dir.path(),
        "hub.rdydump",
    );
    let read = hsrc.engine.read();
    let (v2, v3) = (view_head(&hsrc, "v2"), view_head(&hsrc, "v3"));
    let hub = Hub::new().with_state(|s| s.repo_id = Some(repo_of(&hsrc)));
    hub.views
        .lock()
        .unwrap()
        .insert("v2".into(), (view_root(&read, v2), v2_first));
    hub.views.lock().unwrap().insert(
        "v3".into(),
        (view_root(&read, v3), Hash::of(b"a head past the dump's")),
    );
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &hdump).unwrap();
    assert!(out.holder);
    assert_eq!(
        hub.states.lock().unwrap()[0].views,
        Some(names(&["v1", "v2", "v3"]))
    );
    assert_eq!(
        write_kinds(&hub),
        [
            (WriteKind::Restore, None),
            (WriteKind::View, Some("v1".into())),
            (WriteKind::View, Some("v2".into())),
        ]
    );
    let writes = hub.writes();
    assert_eq!(writes[0].expected, None);
    assert_eq!(writes[1].expected, None);
    assert_eq!(
        writes[2].expected,
        Some(ObjectKey {
            kind: kinds::VIEW_OP,
            hash: v2_first
        })
    );
    let pushes: BTreeSet<&str> = writes.iter().map(|w| w.push.as_str()).collect();
    assert_eq!(pushes.len(), 3, "one push id per write");
    assert_eq!(taken_end(&out.canon).1, hsrc.landings[0]);
    assert_eq!(
        out.views
            .iter()
            .map(|(n, end)| (n.as_str(), matches!(end, WriteEnd::Taken { .. })))
            .collect::<Vec<_>>(),
        [("v1", true), ("v2", true)]
    );
    assert_eq!(out.kept, ["v3"]);
    // the held chain is named by neither canon nor a view: it never travels
    assert!(!hub.sent_keys().contains(&landing(q)));
    assert!(is_empty_dir(scratch.path()));
}

/// D-J3YA0, D-KE83X: a restore of an older dump whose whole history the hub holds still names
/// the root it moves to. The hub tip moved past the dump's head, and known names nothing
/// missing, so one batch of no record names the dump's head as its prerequisite before the
/// write; the write expects the newer tip and canon ends at the dump's head. A view write whose
/// ops the hub holds names its head the same way. A canon or view that is already the dump's
/// sends no batch and no write.
#[test]
fn scc8_restore_of_a_known_older_dump_names_its_root() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let renderer = Renderer::default();

    // canon: the dump is at A, the hub moved on to B and holds every record of both
    let src = env(1);
    let a = src.landings[0];
    let dump = dump_of(&src.engine, dir.path(), "older.rdydump");
    let b = land_in(&src.engine, "newer");
    let hub = Hub::new().with_state(|s| {
        s.repo_id = Some(repo_of(&src));
        s.canon = Some(b);
    });
    *hub.held.lock().unwrap() = closure(&src, b, None);
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);
    assert_eq!(*hub.roots.lock().unwrap(), [(a, vec![landing(a)])]);
    let w = &hub.writes()[0];
    assert_eq!(*hub.puts.lock().unwrap(), [(w.push.clone(), vec![])]);
    assert_eq!(
        (w.kind, w.expected, w.view.clone()),
        (WriteKind::Restore, Some(landing(b)), None)
    );
    assert_eq!(taken_end(&out.canon), (None, a, a));
    assert_eq!(hub.state.lock().unwrap().canon, Some(a));
    assert!(is_empty_dir(scratch.path()));

    // a hub dump: canon is the hub's tip (no batch, no write); the hub holds every op of v1
    // and its head is an earlier op of the dump's chain
    let hsrc = env(1);
    write_in(&hsrc.engine, "v1", "a");
    let v1_first = view_head(&hsrc, "v1");
    write_in(&hsrc.engine, "v1", "b");
    let v1 = view_head(&hsrc, "v1");
    let hdump = hub_dump(&hsrc, &["v1"], None, dir.path(), "hub.rdydump");
    let read = hsrc.engine.read();
    let head = hsrc.landings[0];
    let hub = Hub::new().with_state(|s| {
        s.repo_id = Some(repo_of(&hsrc));
        s.canon = Some(head);
    });
    let ops = rdy_store::closure::history_closure_before(
        read.reader(),
        StreamPurpose::ViewHistory,
        v1,
        &BTreeSet::new(),
        None,
        read.seq(),
    )
    .unwrap()
    .records;
    {
        let mut held = hub.held.lock().unwrap();
        *held = closure(&hsrc, head, None);
        held.extend(ops);
    }
    hub.views
        .lock()
        .unwrap()
        .insert("v1".into(), (view_root(&read, v1), v1_first));
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &hdump).unwrap();
    assert!(matches!(
        out.canon,
        Some(WriteEnd::UpToDate { tip: Some(t) }) if t == head
    ));
    assert_eq!(write_kinds(&hub), [(WriteKind::View, Some("v1".into()))]);
    let op = |hash| ObjectKey {
        kind: kinds::VIEW_OP,
        hash,
    };
    assert_eq!(*hub.roots.lock().unwrap(), [(v1, vec![op(v1)])]);
    let w = &hub.writes()[0];
    assert_eq!(*hub.puts.lock().unwrap(), [(w.push.clone(), vec![])]);
    assert_eq!(w.expected, Some(op(v1_first)));
    match &out.views[..] {
        [(name, WriteEnd::Taken { tip, .. })] => assert_eq!((name.as_str(), *tip), ("v1", v1)),
        other => panic!("{other:?}"),
    }
    assert_eq!(hub.views.lock().unwrap()["v1"].1, v1);

    // the same restore again: canon and v1 are the dump's, so nothing is sent or written
    hub.clear();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &hdump).unwrap();
    assert!(matches!(out.canon, Some(WriteEnd::UpToDate { .. })));
    assert!(matches!(
        &out.views[..],
        [(_, WriteEnd::UpToDate { tip: Some(t) })] if *t == v1
    ));
    assert!(!hub.steps().iter().any(|s| *s == "put" || *s == "write"));
    assert!(is_empty_dir(scratch.path()));
}

/// D-J3YA0 step 5, D-D2NPN, D-R80JP, D-H0BZN "Lost reply": the temporary store is removed after
/// success, a hub refusal, a transport failure, a lost reply (looked up once in the same run), a
/// load refusal and a cancelled call, and the caller's dump keeps its bytes. A hub refusal keeps
/// the hub's words; `E_HUB_TAG_TAKEN` is core's `E_TAG_EXISTS` with them.
#[test]
fn scc8_restore_cleans_temp_store_on_every_exit() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(1);
    let head = src.landings[0];
    let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
    let bytes = std::fs::read(&dump).unwrap();
    let renderer = Renderer::default();
    let check = |what: &str| {
        assert!(
            is_empty_dir(scratch.path()),
            "{what}: a temporary store remains"
        );
        assert_eq!(
            std::fs::read(&dump).unwrap(),
            bytes,
            "{what}: the dump changed"
        );
    };

    // success
    let hub = Hub::new();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    assert_eq!(taken_end(&out.canon).2, head);
    assert!(out.cleanup.is_none());
    check("success");

    // the hub refuses the write: its words stay
    for (code, want) in [
        ("E_HUB_BEHIND", "E_HUB_BEHIND"),
        ("E_HUB_TAG_TAKEN", E_TAG_EXISTS),
    ] {
        let hub = Hub::new();
        hub.plan_write(1, WritePlan::Refuse(hub_words(code)));
        let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
        match &out.canon {
            Some(WriteEnd::Refused {
                push: Some(push),
                error,
            }) => {
                assert_eq!(push.as_str(), hub.writes()[0].push.as_str());
                assert_eq!(
                    (error.code, error.message.as_str(), error.fix.as_str()),
                    (want, "the hub's words", "the hub's fix")
                );
            }
            other => panic!("{other:?}"),
        }
        check(code);
    }

    // a transport failure
    let hub = Hub::new();
    hub.fail_at("put", 1, transport_failure());
    let e = err(restore_with(
        &caller,
        &hub,
        &renderer,
        scratch.path(),
        &dump,
    ));
    assert_eq!(e.code, E_UPSTREAM_TRANSPORT);
    check("transport failure");

    // a lost reply of a write that took: the outcome lookup in the same run reports it (one
    // lookup when the hub answers at once, as here; busy answers are waited out, r59016)
    let hub = Hub::new();
    hub.plan_write(1, WritePlan::Lost { apply: true });
    let tap = Tap::new(&hub, true);
    let out = restore_with(&caller, &tap, &renderer, scratch.path(), &dump).unwrap();
    assert_eq!(hub.steps().iter().filter(|s| **s == "outcome").count(), 1);
    assert_eq!(taken_end(&out.canon).2, head);
    assert_eq!(out.lookups.len(), 1);
    assert_eq!(out.lookups[0].push, hub.writes()[0].push);
    assert!(matches!(
        out.lookups[0].outcome,
        LookupOutcome::Taken { .. }
    ));
    check("lost reply, taken");

    // a lost reply the hub knows no outcome of: publish's uncertain words naming the push id;
    // the write is never sent again
    let hub = Hub::new();
    hub.plan_write(1, WritePlan::Lost { apply: false });
    let e = err(restore_with(
        &caller,
        &hub,
        &renderer,
        scratch.path(),
        &dump,
    ));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            UPSTREAM_UNCERTAIN_MESSAGE,
            UPSTREAM_UNCERTAIN_FIX
        )
    );
    let push = hub.writes()[0].push.clone();
    assert!(
        e.issues.iter().any(|i| i.message.contains(&push)),
        "{:?}",
        e.issues
    );
    assert_eq!(hub.writes().len(), 1);
    assert_eq!(hub.steps().iter().filter(|s| **s == "outcome").count(), 1);
    check("lost reply, unknown");

    // a dump the load refuses
    let bad = dir.path().join("bad.rdydump");
    std::fs::write(&bad, b"not a dump").unwrap();
    let hub = Hub::new();
    let e = err(restore_with(&caller, &hub, &renderer, scratch.path(), &bad));
    assert_eq!(e.code, E_DUMP);
    assert!(hub.steps().is_empty());
    assert_eq!(std::fs::read(&bad).unwrap(), b"not a dump");
    check("load refusal");

    // a cancelled call: the restore is dropped during its put
    let hub = Hub::new();
    *hub.hook.lock().unwrap() = Some(("put", Box::new(|_| panic!("the call is cancelled"))));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        restore_with(&caller, &hub, &renderer, scratch.path(), &dump)
    }));
    assert!(r.is_err());
    check("cancelled");
}

/// D-D5J18, D-QMZ92, D-8RGWW, D-0WWYR: a project dump's restore captures its gate settings and
/// render inputs from the dump's own project files, with this client as `rdy`: the dump's title,
/// description and effective pack, and its gate with that pack's digest. The caller project's
/// settings never enter, and neither the dump writer's version nor the hub's is compared.
#[test]
fn scc8_restore_uses_only_dump_project_files_for_gate_and_render_inputs() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    append(
        &caller.project.join(".rdy.yaml"),
        "name: Caller Title\ndescription: The caller's words.\ncoverage:\n  target: 7\npropose:\n  cadence: asked\n",
    );
    let src = env(1);
    append(
        &src.project.join(".rdy.yaml"),
        "name: Dump Title\ndescription: The dump's words.\ncoverage:\n  target: 42\n  gate: no_lower\npropose:\n  cadence: task\n",
    );
    let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
    // what the dump's own files give, read from a plain load of it
    let plain = dir.path().join("plain");
    rdy_engine::load::load(&dump, &plain).unwrap();
    let loaded = Rdy::open(&plain).unwrap();
    let lread = loaded.read();
    let want_inputs = rdy_engine::render_inputs::capture(&lread).unwrap();
    let want = CheckInputs::of_state(&lread);
    assert_eq!(want.coverage_target, 42);

    let hub = Hub::new().with_state(|s| s.rdy = "0.1.0".into());
    let renderer = Renderer::default();
    restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    let w = &hub.writes()[0];
    let inputs = w
        .render_inputs
        .clone()
        .expect("a dump that names its title sends render inputs");
    assert_eq!(
        semver::Version::parse(&inputs.rdy).unwrap(),
        semver::Version::parse(&rdy_engine::formats::formats().rdy).unwrap()
    );
    assert_eq!(
        (inputs.title.as_str(), inputs.description.as_deref()),
        ("Dump Title", Some("The dump's words."))
    );
    assert_eq!(inputs.pack, want_inputs.pack);
    assert_eq!(
        w.gate,
        Some(GateSettings {
            coverage_gate: want.coverage_gate.as_str().into(),
            coverage_target: 42,
            decisions_home: want.decisions_home.clone(),
            pack_digest: pack_digest(&want_inputs.pack),
            propose: ProposeGate {
                cadence: "task".into(),
                changes: want.propose_changes,
            },
        })
    );
    // the caller's settings never enter
    let body = serde_json::to_string(w).unwrap();
    assert!(!body.contains("Caller Title") && !body.contains("The caller's words."));
    // one capture, at the dump's head
    assert_eq!(
        *renderer.scopes.lock().unwrap(),
        [Scope::At(src.landings[0])]
    );
}

/// D-D5J18 "Never invented": a dump whose `.rdy.yaml` names no title sends `render_inputs`
/// null, still sends its gate settings (the pack by its digest), and lists each tag it sent
/// without render inputs. No request names the temporary directory.
#[test]
fn scc8_restore_without_explicit_title_omits_render_inputs_and_lists_tags() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(1);
    tag_head(&src, "v1.0.0");
    let yaml = std::fs::read_to_string(src.project.join(".rdy.yaml")).unwrap();
    assert!(!yaml.contains("name:"), "{yaml}");
    let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
    let hub = Hub::new();
    // the temporary directory's name, read while the restore runs
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let seen = Arc::clone(&seen);
        let root = scratch.path().to_path_buf();
        *hub.hook.lock().unwrap() = Some((
            "write",
            Box::new(move |_| {
                for entry in std::fs::read_dir(&root).unwrap() {
                    let name = entry.unwrap().file_name().to_string_lossy().to_string();
                    seen.lock().unwrap().push(name);
                }
            }),
        ));
    }
    let renderer = Renderer::default();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
    let w = &hub.writes()[0];
    assert_eq!(w.render_inputs, None);
    let gate = w.gate.clone().expect("the dump's gate settings still go");
    assert_eq!(gate.pack_digest, pack_digest(src.engine.read().pack()));
    assert_eq!(
        w.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["v1.0.0"]
    );
    assert_eq!(out.without_inputs, ["v1.0.0"]);
    let body = serde_json::to_value(w).unwrap();
    assert!(body["render_inputs"].is_null());
    // no request names the temporary directory, as it is or as a title
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let temp = seen[0].to_lowercase();
    let as_title = temp.replace(['-', '_'], " ").trim().to_string();
    let mut bodies: Vec<String> = hub
        .writes()
        .iter()
        .map(|w| serde_json::to_string(w).unwrap())
        .collect();
    bodies.extend(
        hub.states
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::to_string(s).unwrap()),
    );
    for b in bodies {
        let b = b.to_lowercase();
        assert!(!b.contains(&temp) && !b.contains(&as_title), "{b}");
    }
}

/// D-D5J18 "Restore from a hub dump": a holder dump has no project files, so its canon write and
/// every view write send `gate` null and `render_inputs` null, and nothing is captured.
#[test]
fn scc8_holder_restore_sends_no_gate_or_render_inputs() {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(1);
    tag_head(&src, "v1.0.0");
    write_in(&src.engine, "v1", "a");
    let hdump = hub_dump(&src, &["v1"], None, dir.path(), "hub.rdydump");
    let hub = Hub::new();
    let renderer = Renderer::default();
    let out = restore_with(&caller, &hub, &renderer, scratch.path(), &hdump).unwrap();
    assert!(out.holder);
    assert_eq!(
        write_kinds(&hub),
        [
            (WriteKind::Restore, None),
            (WriteKind::View, Some("v1".into()))
        ]
    );
    for w in hub.writes() {
        assert_eq!((w.gate.clone(), w.render_inputs.clone()), (None, None));
        let body = serde_json::to_value(&w).unwrap();
        assert!(body["gate"].is_null() && body["render_inputs"].is_null());
    }
    assert!(renderer.scopes.lock().unwrap().is_empty());
    assert_eq!(
        hub.writes()[0]
            .tags
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["v1.0.0"]
    );
    assert_eq!(out.without_inputs, ["v1.0.0"]);
}

/// SD1 (D-8T7PA): every restore batch carries each landing's `SNAPSHOT_DELTA` (kind 30) and the
/// `snapshot_delta` feature minimum, and passes core's batch check, from a project dump and from
/// a hub dump.
#[test]
fn scc8_restore_preserves_sd1_deltas() {
    use rdy_store::format::FEATURE_SNAPSHOT_DELTA;
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(2);
    write_in(&src.engine, "v1", "a");
    let dumps = [
        dump_of(&src.engine, dir.path(), "project.rdydump"),
        hub_dump(&src, &["v1"], None, dir.path(), "hub.rdydump"),
    ];
    let repo = repo_of(&src);
    for dump in &dumps {
        let hub = Hub::new();
        let tap = Tap::new(&hub, false);
        let renderer = Renderer::default();
        restore_with(&caller, &tap, &renderer, scratch.path(), dump).unwrap();
        let batches = tap.batches.lock().unwrap().clone();
        assert!(!batches.is_empty());
        let mut landings = BTreeSet::new();
        for b in &batches {
            let report = stream_check::check_batch(&b[..], Some(repo)).unwrap();
            assert!(
                report.header.features.has(FEATURE_SNAPSHOT_DELTA),
                "{:?}",
                report.header.features
            );
            let here: Vec<&ObjectKey> = report
                .keys
                .iter()
                .filter(|k| k.kind == kinds::LANDING)
                .collect();
            let deltas = report
                .keys
                .iter()
                .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
                .count();
            assert_eq!(deltas, here.len(), "one delta for each landing");
            landings.extend(here.into_iter().map(|k| k.hash));
        }
        assert_eq!(
            landings,
            src.landings.iter().copied().collect::<BTreeSet<_>>()
        );
    }
}

/// The names of the TAG records `tap`'s put batches carried, sorted, and empties the tap.
fn carried_tags(tap: &Tap<'_>, read: &ReadState) -> Vec<String> {
    let batches = std::mem::take(&mut *tap.batches.lock().unwrap());
    let mut names = Vec::new();
    for b in &batches {
        let report = stream_check::check_batch(&b[..], None).unwrap();
        for k in report.keys.iter().filter(|k| k.kind == kinds::TAG) {
            names.push(read.reader().read_tag(&k.hash).unwrap().name);
        }
    }
    names.sort();
    names
}

/// The tag names of `write`, as sent.
fn named_tags(write: &SyncWrite) -> Vec<String> {
    write.tags.iter().map(|t| t.name.clone()).collect()
}

/// rdyhub d84d2d96: the hub refuses a write that does not name every tag its push carries, or
/// that names one twice. A push's write and a restore's write name each carried tag once: a
/// seed's every tag, an extend's new tag only.
#[test]
fn scc8_every_write_names_each_carried_tag_once() {
    let e = env(1);
    tag_head(&e, "v1.0.0");
    let hub = Hub::new();
    let tap = Tap::new(&hub, false);
    let s = Sync::new(&e, &hub);
    let service = SyncService::new(
        &e.project,
        &e.user_file,
        &s.api,
        &s.clock,
        &tap,
        &s.renderer,
    );
    // a seed carries v1.0.0
    service
        .push(&e.engine, &agent(), PushRequest::default())
        .unwrap();
    let read = e.engine.read();
    assert_eq!(carried_tags(&tap, &read), ["v1.0.0"]);
    assert_eq!(named_tags(&hub.writes()[0]), ["v1.0.0"]);
    // an extend carries only the new v1.1.0 and v1.2.0
    land_in(&e.engine, "two");
    tag_head(&e, "v1.1.0");
    land_in(&e.engine, "three");
    tag_head(&e, "v1.2.0");
    service
        .push(&e.engine, &agent(), PushRequest::default())
        .unwrap();
    let read = e.engine.read();
    assert_eq!(carried_tags(&tap, &read), ["v1.1.0", "v1.2.0"]);
    assert_eq!(named_tags(&hub.writes()[1]), ["v1.1.0", "v1.2.0"]);

    // a restore into an empty hub repo carries and names all three, once each
    let dir = private_dir();
    let scratch = private_dir();
    let dump = dump_of(&e.engine, dir.path(), "tags.rdydump");
    let caller = env(0);
    let fresh = Hub::new();
    let tap = Tap::new(&fresh, false);
    let renderer = Renderer::default();
    restore_with(&caller, &tap, &renderer, scratch.path(), &dump).unwrap();
    assert_eq!(carried_tags(&tap, &read), ["v1.0.0", "v1.1.0", "v1.2.0"]);
    let writes = fresh.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(named_tags(&writes[0]), ["v1.0.0", "v1.1.0", "v1.2.0"]);
}

/// D-SEGWK: a push's objects/put bodies go as zstd when the sync state reply holds
/// `zstd: true`, as a publish's do, and only then: every request after the state step says so.
#[test]
fn upfast_push_puts_follow_the_state_reply_zstd_member() {
    for zstd in [false, true] {
        let e = env(2);
        let hub = Hub::new().with_state(|s| s.zstd = zstd);
        push(&e, &hub, None).unwrap();
        let calls = hub.calls();
        assert!(calls.iter().any(|c| c.step == "put"), "{zstd}");
        for c in calls.iter().filter(|c| matches!(c.step, "known" | "put")) {
            assert_eq!(c.zstd, zstd, "{zstd}: {}", c.step);
        }
    }
}

/// D-SEGWK: a restore's objects/put bodies follow the state reply's `zstd` member too.
#[test]
fn upfast_restore_puts_follow_the_state_reply_zstd_member() {
    for zstd in [false, true] {
        let dir = private_dir();
        let scratch = private_dir();
        let caller = env(0);
        let src = env(2);
        let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
        let renderer = Renderer::default();
        let hub = Hub::new().with_state(|s| s.zstd = zstd);
        restore_with(&caller, &hub, &renderer, scratch.path(), &dump).unwrap();
        let calls = hub.calls();
        assert!(calls.iter().any(|c| c.step == "put"), "{zstd}");
        for c in calls.iter().filter(|c| matches!(c.step, "known" | "put")) {
            assert_eq!(c.zstd, zstd, "{zstd}: {}", c.step);
        }
    }
}

/// The hub's history of a store that lacks `later` landings of it: the project, the hub's
/// fake serving the longer store, the keys the pull must fetch, and the new tip.
struct Lacking {
    e: Env,
    hub: Hub,
    tip: Hash,
    want: Vec<ObjectKey>,
    _dir: tempfile::TempDir,
    _hub_store: Local,
}

fn lacking(later: usize) -> Lacking {
    let e = env(2);
    let (dir, hub_store) = copy_project(&e);
    let mut tip = e.landings[1];
    for i in 0..later {
        tip = land_in(&hub_store, &format!("r59023-{i}"));
    }
    let hub = Hub::new();
    hub.serve(hub_store.read());
    hub.state.lock().unwrap().canon = Some(tip);
    let read = e.engine.read();
    let want: Vec<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .filter(|k| !read.reader().has_kind(k.kind, &k.hash).unwrap())
        .collect();
    drop(read);
    Lacking {
        e,
        hub,
        tip,
        want,
        _dir: dir,
        _hub_store: hub_store,
    }
}

/// A get that the hub refuses over its cap, with the structured marker, is split in two as a
/// get over `batch_bytes` is: each refused group's front half is asked next, the halves are
/// fetched in turn, and the fetch completes with the hub's history.
#[test]
fn r59023_get_refused_over_the_cap_halves_and_completes() {
    const CAP: u64 = 8_192;
    let l = lacking(20);
    let (e, hub) = (&l.e, &l.hub);
    *hub.refuse_get_over.lock().unwrap() = Some((CAP, true));
    Sync::new(e, hub).pull(PullRequest::default()).unwrap();
    let gets = hub.gets.lock().unwrap().clone();
    let replies = hub.get_replies.lock().unwrap().clone();
    assert_eq!(gets.len(), replies.len());
    let refused: Vec<usize> = (0..gets.len()).filter(|&i| replies[i].0 > CAP).collect();
    assert!(!refused.is_empty(), "{replies:?}");
    for &i in &refused {
        let group = &gets[i].keys;
        assert!(group.len() >= 2, "{group:?}");
        let (front, _) = group.split_at(group.len() / 2);
        assert_eq!(gets[i + 1].keys, front, "get {i}");
        assert_eq!(gets[i + 1].purpose, gets[i].purpose);
        assert_eq!(gets[i + 1].root, gets[i].root);
    }
    // the gets that were served ask for every wanted key once, in the hub's order
    let kept: Vec<ObjectKey> = (0..gets.len())
        .filter(|&i| replies[i].0 <= CAP)
        .flat_map(|i| gets[i].keys.clone())
        .collect();
    assert_eq!(kept, l.want);
    assert!(gets.len() < 2 * l.want.len(), "{}", gets.len());
    let read = e.engine.read();
    for k in &l.want {
        assert!(read.reader().has_kind(k.kind, &k.hash).unwrap(), "{k:?}");
    }
    assert!(read.reader().has_kind(kinds::LANDING, &l.tip).unwrap());
    drop(read);
    assert_eq!(canon_of(e, "shared").unwrap().fetched, Some(l.tip));
}

/// The same refusal with no marker ends the fetch after one get with the hub's code, message and
/// fix, and nothing of the history is stored. A refusal with the marker of a one-key group ends
/// it with the hub's error unchanged, after the halving has reached that key.
#[test]
fn r59023_refusal_without_the_marker_ends_the_fetch() {
    const CAP: u64 = 8_192;
    let l = lacking(20);
    let (e, hub) = (&l.e, &l.hub);
    *hub.refuse_get_over.lock().unwrap() = Some((CAP, false));
    let x = err(Sync::new(e, hub).pull(PullRequest::default()));
    assert_eq!(x.code, "E_HUB_VALIDATION");
    assert_eq!(
        x.message,
        format!("the records you asked for are larger than {CAP} bytes")
    );
    assert_eq!(x.fix, "ask for fewer records in each get");
    assert_eq!(x.hub_get_cap, None);
    assert_eq!(hub.gets.lock().unwrap().len(), 1);
    // nothing was fetched: the tip is not held and no fetched tip is recorded
    let read = e.engine.read();
    assert!(!read.reader().has_kind(kinds::LANDING, &l.tip).unwrap());
    drop(read);
    assert_eq!(canon_of(e, "shared").and_then(|c| c.fetched), None);

    // with the marker, every get is refused: the halving reaches one key and the hub's error
    // ends the fetch, unchanged
    *hub.refuse_get_over.lock().unwrap() = Some((1, true));
    hub.gets.lock().unwrap().clear();
    hub.get_replies.lock().unwrap().clear();
    let x = err(Sync::new(e, hub).pull(PullRequest::default()));
    assert_eq!(x.code, "E_HUB_VALIDATION");
    assert_eq!(
        x.message,
        "the records you asked for are larger than 1 bytes"
    );
    assert_eq!(x.fix, "ask for fewer records in each get");
    assert_eq!(x.hub_get_cap, Some(1));
    let gets = hub.gets.lock().unwrap().clone();
    // each refusal asks next for the front half of its group, down to one key
    let mut n = gets[0].keys.len();
    assert!(n >= 2, "{n}");
    let mut steps = 1;
    while n > 1 {
        n /= 2;
        steps += 1;
    }
    assert_eq!(gets.len(), steps);
    assert_eq!(gets.last().unwrap().keys.len(), 1);
    let read = e.engine.read();
    assert!(!read.reader().has_kind(kinds::LANDING, &l.tip).unwrap());
    drop(read);
    assert_eq!(canon_of(e, "shared").and_then(|c| c.fetched), None);
}

// ---------------------------------------------------------------------------------------------
// The canon capture beside the walk, and the empty hub's seed
// ---------------------------------------------------------------------------------------------

/// How long a channel wait may take before the test fails instead of hanging. No assertion
/// reads elapsed time.
const STUCK: Duration = Duration::from_secs(120);

/// The capture's side of a rendezvous with the walk: it signals, then waits for the walk.
type Meet = (Mutex<mpsc::Sender<()>>, Mutex<mpsc::Receiver<()>>);

/// A renderer for the canon capture: at its first capture it runs `before`, then meets the
/// caller's walk at a rendezvous when `meet` is set, then returns `fail`, or `fixed`, or what
/// [`Renderer`] captures. `done` is set when that first capture returns.
#[derive(Default)]
struct CanonRenderer<'a> {
    inner: Renderer,
    before: Option<Box<dyn Fn() + Send + std::marker::Sync + 'a>>,
    meet: Option<Meet>,
    fail: Option<RdyError>,
    panic: bool,
    fixed: Option<RenderInputs>,
    first: AtomicBool,
    done: Arc<AtomicBool>,
}

impl PublishRenderer for CanonRenderer<'_> {
    fn capture(&self, read: &ReadState, scope: &Scope) -> Result<RenderInputs, RdyError> {
        if self.first.swap(true, Ordering::SeqCst) {
            return self.inner.capture(read, scope);
        }
        if let Some(f) = &self.before {
            f();
        }
        if let Some((tx, rx)) = &self.meet {
            tx.lock().unwrap().send(()).unwrap();
            rx.lock()
                .unwrap()
                .recv_timeout(STUCK)
                .expect("the walk starts while the capture runs");
        }
        if self.panic {
            panic!("SENTINEL-PANIC {}", access());
        }
        let out = match (&self.fail, &self.fixed) {
            (Some(e), _) => Err(e.clone()),
            (None, Some(inputs)) => Ok(inputs.clone()),
            (None, None) => self.inner.capture(read, scope),
        };
        self.done.store(true, Ordering::SeqCst);
        out
    }

    fn render(
        &self,
        _: &ReadState,
        _: &Scope,
        _: &str,
        _: &str,
        _: &RenderInputs,
    ) -> Result<RenderedUpload, RdyError> {
        panic!("a sync renders nothing")
    }
}

/// A push of `e`'s canon through `hub` with `renderer`.
fn push_rendered(
    e: &Env,
    hub: &Hub,
    renderer: &(dyn PublishRenderer + std::marker::Sync),
    request: PushRequest,
) -> Result<PushOutcome, RdyError> {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    SyncService::new(&e.project, &e.user_file, &api, &clock, hub, renderer).push(
        &e.engine,
        &agent(),
        request,
    )
}

/// Lands a doc with one prose block in `engine`; returns the block's id.
fn land_prose(engine: &Local) -> String {
    rdy_engine::engine::with_writer_result(engine, |r| {
        let w = r.write(
            &writer(),
            rdy_engine::WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(serde_json::json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "secret", "title": "Secret", "doc_type": "page"}},
                    {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."}
                ]))
                .unwrap(),
                justification: None,
                message: "r5907".into(),
            },
        )?;
        let p = r.propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))?;
        common::confirm_exceptions(r, &founder(), &p);
        r.land(&founder(), &p.id)?;
        Ok(w.created["$p"].clone())
    })
    .unwrap()
}

/// Removes every version of block `target` of `engine` through the store's own landing path.
fn remove_in(engine: &Local, target: &str) {
    let target = target.to_string();
    rdy_engine::engine::with_writer_result(engine, move |r| {
        let s = writer();
        let req = r.redact(
            &s,
            rdy_engine::redaction::RedactInput {
                targets: vec![target],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )?;
        assert!(req.redaction.is_some());
        let p = r.propose(&s, "work", "Remove the code", None)?;
        let version: Hash = p.id.parse().unwrap();
        r.seal_open()?;
        r.store_mut().set_held(false)?;
        r.store_mut().land_atomic(rdy_store::LandRequest {
            proposal: version,
            accept: None,
            approver: rdy_model::Provenance {
                actor: "human:t".into(),
                run: None,
                role: rdy_model::Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })?;
        r.store_mut().set_held(true)?;
        r.invalidate();
        Ok(())
    })
    .unwrap()
}

/// Overwrites one byte in the middle of `kind` record `hash`'s stored payload in `e`.
fn corrupt_in(e: &Env, kind: u8, hash: &Hash) {
    use std::io::{Seek, SeekFrom, Write};
    let read = e.engine.read();
    let (_, payload) = read.reader().get_kind_raw(kind, hash).unwrap();
    let segments = read.reader().dir().join("segments");
    let mut hits = Vec::new();
    for entry in std::fs::read_dir(&segments).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        if let Some(at) = bytes.windows(payload.len()).position(|w| w == &payload[..]) {
            hits.push((path, at));
        }
    }
    assert_eq!(hits.len(), 1, "the payload is stored once");
    let (path, at) = &hits[0];
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let mid = payload.len() / 2;
    f.seek(SeekFrom::Start((*at + mid) as u64)).unwrap();
    f.write_all(&[payload[mid] ^ 0xa5]).unwrap();
    f.sync_all().unwrap();
}

fn capture_error() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        "the injected capture refusal",
        "the test's fix",
    )
}

#[track_caller]
fn envelope(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// D-3PNRK ruling 1: the canon capture runs on one worker beside the caller's walk, and both
/// meet before either ends; the worker joins before the first put, and only one worker runs at
/// a time (the batch preparation's starts after the capture's joined).
#[test]
fn r5907_capture_overlaps_walk_and_joins_before_send() {
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let hub = Hub::new().with_state(|s| s.batch_records = 2);
    let (to_walk, at_walk) = mpsc::channel();
    let (to_capture, at_capture) = mpsc::channel();
    let renderer = CanonRenderer {
        meet: Some((Mutex::new(to_walk), Mutex::new(at_capture))),
        ..CanonRenderer::default()
    };
    let done = renderer.done.clone();
    let walk: Arc<dyn Fn() + Send + std::marker::Sync> = {
        let (to_capture, at_walk) = (Mutex::new(to_capture), Mutex::new(at_walk));
        Arc::new(move || {
            to_capture.lock().unwrap().send(()).unwrap();
            at_walk
                .lock()
                .unwrap()
                .recv_timeout(STUCK)
                .expect("the capture runs while the walk starts");
        })
    };
    let captured_before_put = Arc::new(Mutex::new(None));
    {
        let (done, seen) = (done.clone(), captured_before_put.clone());
        *hub.hook.lock().unwrap() = Some((
            "put",
            Box::new(move |_| *seen.lock().unwrap() = Some(done.load(Ordering::SeqCst))),
        ));
    }
    // the worker setup reaches the capture's and the preparation's workers
    let workers = Arc::new(Mutex::new(Vec::new()));
    let setup: rdy_engine::hub_publish_stream::UploadWorkerSetup = {
        let workers = workers.clone();
        Arc::new(move || {
            let name = std::thread::current().name().unwrap_or("").to_string();
            workers.lock().unwrap().push(name);
            Box::new(())
        })
    };
    let probe = Arc::new(UploadProbe::default());
    let out = with_upload_worker_setup_for_tests(setup, || {
        with_upload_probe_for_tests(probe.clone(), || {
            with_canon_walk_hook_for_tests(walk, || {
                push_rendered(&e, &hub, &renderer, PushRequest::default())
            })
        })
    })
    .unwrap();
    let (_, base, pushed, _, _, sent) = taken(&out);
    assert_eq!((base, pushed), (None, head));
    assert!(sent.batches >= 2, "{sent:?}");
    assert_eq!(*captured_before_put.lock().unwrap(), Some(true));
    let steps = hub.steps();
    assert_eq!(&steps[..3], ["discovery", "state", "put"]);
    assert_eq!(steps.last(), Some(&"write"));
    assert_eq!(
        *workers.lock().unwrap(),
        ["rdy-canon-capture", "rdy-upload-prepare"]
    );
    assert_eq!(probe.workers_started.load(Ordering::SeqCst), 2);
    assert_eq!(probe.max_workers.load(Ordering::SeqCst), 1);
    assert_eq!(probe.live_workers.load(Ordering::SeqCst), 0);
    // one capture of canon, as before
    assert_eq!(renderer.inner.scopes.lock().unwrap().len(), 1);
}

/// D-3PNRK ruling 1: when the capture and the walk both fail, the capture's error is returned;
/// a capture that succeeds never hides a failed walk; a capture panic is core's own words. Each
/// joins its worker, and nothing is sent or written.
#[test]
fn r5907_capture_failure_precedes_walk_failure() {
    for case in ["both", "walk only", "panic"] {
        let e = env(2);
        let block = land_prose(&e.engine);
        let fixed = rdy_engine::render_inputs::capture(&e.engine.read()).unwrap();
        let (removed, at_walk) = mpsc::channel();
        let engine = &e.engine;
        let renderer = CanonRenderer {
            // the removal lands while the walk waits, so the walk's reads fail
            before: Some(Box::new(move || {
                remove_in(engine, &block);
                removed.send(()).unwrap();
            })),
            fail: (case == "both").then(capture_error),
            panic: case == "panic",
            fixed: Some(fixed),
            ..CanonRenderer::default()
        };
        let walk: Arc<dyn Fn() + Send + std::marker::Sync> = {
            let at_walk = Mutex::new(at_walk);
            Arc::new(move || {
                at_walk.lock().unwrap().recv_timeout(STUCK).unwrap();
            })
        };
        let hub = Hub::new();
        let probe = Arc::new(UploadProbe::default());
        let got = with_upload_probe_for_tests(probe.clone(), || {
            with_canon_walk_hook_for_tests(walk, || {
                push_rendered(&e, &hub, &renderer, PushRequest::default())
            })
        });
        let x = err(got);
        match case {
            "both" => assert_eq!(envelope(&x), envelope(&capture_error())),
            "walk only" => assert_eq!(x.code, E_REDACTED_STATE, "{x}"),
            _ => {
                assert_eq!(
                    envelope(&x),
                    (E_INTERNAL, CAPTURE_FAILED_MESSAGE, CAPTURE_FAILED_FIX)
                );
                assert!(!format!("{x:?}").contains("SENTINEL"));
                assert!(!format!("{x:?}").contains(&access()));
            }
        }
        assert_eq!(hub.steps(), ["discovery", "state"], "{case}");
        assert_eq!(probe.workers_started.load(Ordering::SeqCst), 1, "{case}");
        assert_eq!(probe.live_workers.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(upload_of(&e, "shared"), None, "{case}");
        assert_eq!(pending_of(&e, "shared"), None, "{case}");
    }
}

/// The captured read stays the one the push sends: a landing during the capture is left out,
/// and a removal during it refuses the push.
#[test]
fn r5907_capture_and_epoch_guards_survive() {
    // a later append is excluded
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let engine = &e.engine;
    let later = Arc::new(Mutex::new(None));
    let renderer = CanonRenderer {
        before: Some(Box::new({
            let later = later.clone();
            move || *later.lock().unwrap() = Some(land_in(engine, "later"))
        })),
        ..CanonRenderer::default()
    };
    let hub = Hub::new();
    let out = push_rendered(&e, &hub, &renderer, PushRequest::default()).unwrap();
    let (_, base, pushed, tip, _, _) = taken(&out);
    assert_eq!((base, pushed, tip), (None, head, head));
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, None));
    let later = later.lock().unwrap().unwrap();
    assert!(!sent_keys.contains(&landing(later)));
    assert_eq!(e.engine.read().canon_head(), Some(later));
    assert_eq!(hub.state.lock().unwrap().canon, Some(head));

    // a later removal refuses before anything is sent
    let e = env(2);
    let block = land_prose(&e.engine);
    let engine = &e.engine;
    let renderer = CanonRenderer {
        before: Some(Box::new(move || remove_in(engine, &block))),
        ..CanonRenderer::default()
    };
    let hub = Hub::new();
    let x = err(push_rendered(&e, &hub, &renderer, PushRequest::default()));
    assert_eq!(x.code, E_REDACTED_STATE, "{x}");
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(upload_of(&e, "shared"), None);
}

/// D-3PNRK ruling 1: a new canon push to an empty hub (no repo id, no canon, no view) sends its
/// first put with no known step; a held repo id, a resumed push and a view keep known. A bad
/// record refuses before any put, and a racing seed loses the hub's compare.
#[test]
fn r5907_seed_skips_known_only_when_empty() {
    // empty and new: put first, every key travels
    let e = env(2);
    let head = *e.landings.last().unwrap();
    let hub = Hub::new();
    taken(&push(&e, &hub, None).unwrap());
    assert_eq!(hub.steps(), ["discovery", "state", "put", "write"]);
    let sent_keys: BTreeSet<ObjectKey> = hub.sent_keys().into_iter().collect();
    assert_eq!(sent_keys, closure(&e, head, None));

    // a hub that holds the repo but no canon keeps known
    let e = env(2);
    let repo = repo_of(&e);
    let hub = Hub::new().with_state(|s| s.repo_id = Some(repo));
    taken(&push(&e, &hub, None).unwrap());
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);

    // a view keeps known, after canon's seed
    let e = env(1);
    write_in(&e.engine, "alpha", "a1");
    let hub = Hub::new();
    Sync::new(&e, &hub)
        .push(PushRequest {
            views: names(&["alpha"]),
            ..PushRequest::default()
        })
        .unwrap();
    assert_eq!(
        hub.steps(),
        [
            "discovery",
            "state",
            "put",
            "write",
            "known",
            "put",
            "write"
        ]
    );
    assert_eq!(
        write_kinds(&hub),
        [
            (WriteKind::Landings, None),
            (WriteKind::View, Some("alpha".into()))
        ]
    );

    // a resumed push keeps known, under the same push id
    let e = env(2);
    let hub = Hub::new().with_state(|s| s.batch_records = 2);
    hub.fail_at("put", 2, transport_failure());
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, E_UPSTREAM_TRANSPORT, "{x}");
    let first = hub.calls()[2].push.clone();
    assert_eq!(hub.steps()[..3], ["discovery", "state", "put"]);
    let slot = upload_of(&e, "shared").expect("the upload slot stays");
    hub.clear();
    let out = push(&e, &hub, None).unwrap();
    let (id, _, _, _, resumed, _) = taken(&out);
    assert!(resumed);
    assert_eq!(Some(id.clone()), first);
    assert_eq!(slot.push, id);
    assert_eq!(hub.steps()[..3], ["discovery", "state", "known"]);

    // a bad record refuses before any put
    let e = env(2);
    let landing_record: rdy_model::LandingRecord = e
        .engine
        .read()
        .reader()
        .get_typed(kinds::LANDING, &e.landings[0])
        .unwrap();
    corrupt_in(&e, kinds::CHANGE, &landing_record.changes[0]);
    let hub = Hub::new();
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, E_IO, "{x}");
    assert_eq!(hub.steps(), ["discovery", "state"]);
    assert_eq!(upload_of(&e, "shared"), None);

    // a seed racing another loses the hub's compare: canon is unchanged and nothing retries
    let e = env(2);
    let hub = Hub::new();
    hub.plan_write(1, WritePlan::Refuse(hub_words("E_HUB_BEHIND")));
    let head = e.engine.read().canon_head();
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Refused { error, .. } = &out.canon else {
        panic!("{out:?}")
    };
    assert_eq!(error.code, "E_HUB_BEHIND");
    assert_eq!(hub.steps(), ["discovery", "state", "put", "write"]);
    assert_eq!(hub.writes().len(), 1);
    assert_eq!(canon_of(&e, "shared"), None);
    assert_eq!(e.engine.read().canon_head(), head);
    assert_eq!(hub.state.lock().unwrap().canon, None);
}

// ---------------------------------------------------------------------------------------------
// Busy outcome lookups (r59016: D-GS59E, D-H0BZN, D-J3YA0)
// ---------------------------------------------------------------------------------------------

/// The quarantine life these tests give the fake hub, in seconds and milliseconds. The sign-in
/// lasts an hour, so a clock two lives on still holds a valid access.
const QUARANTINE_S: u64 = 60;
const QUARANTINE_MS: i64 = 60_000;

/// Scripts the next `n` outcome lookups of `hub` as busy refusals that ask for `seconds`.
fn script_busy(hub: &Hub, seconds: u64, n: usize) {
    let seen = hub.steps().iter().filter(|s| **s == "outcome").count();
    for i in 1..=n {
        hub.fail_at("outcome", seen + i, busy_refusal(seconds));
    }
}

/// What the hub answers to the lookup of the write it saw last.
#[derive(Clone, Copy)]
enum Last {
    Taken,
    /// Taken, with a tip of this kind in place of the write's own.
    TakenAs(u8),
    Held(u64),
    Refused(&'static str),
}

/// Makes every outcome lookup of `hub` that no refusal scripts answer as `last` for the latest
/// write the hub received (its push id is not known before the run).
fn answer_last_write(hub: &Arc<Hub>, last: Last) {
    let weak = Arc::downgrade(hub);
    *hub.each.lock().unwrap() = Some((
        "outcome",
        Box::new(move |_| {
            let h = weak.upgrade().expect("the hub is alive");
            let w = h.writes().last().cloned().expect("a write came first");
            let outcome = match last {
                Last::Taken | Last::TakenAs(_) => {
                    let tip = match &w.view {
                        None => h.state.lock().unwrap().canon,
                        Some(name) => h.views.lock().unwrap().get(name).map(|v| v.1),
                    }
                    .expect("the write moved the hub");
                    let kind = match last {
                        Last::TakenAs(kind) => kind,
                        _ => w.root_kind(),
                    };
                    LookupOutcome::Taken {
                        tip: ObjectKey { kind, hash: tip },
                    }
                }
                Last::Held(number) => LookupOutcome::Held {
                    request: HubRequest { number, version: 1 },
                    tip: None,
                },
                Last::Refused(code) => LookupOutcome::Refused { code: code.into() },
            };
            h.answer(&w.push, outcome);
        }),
    ));
}

/// Advances `poll` by `seconds` at the start of every outcome lookup of `hub`: a lookup that
/// takes that long.
fn lookup_costs(hub: &Hub, poll: &Arc<PollClock>, seconds: u64) {
    let poll = Arc::clone(poll);
    *hub.each.lock().unwrap() = Some((
        "outcome",
        Box::new(move |_| poll.advance(Duration::from_secs(seconds))),
    ));
}

/// Seeds a sent write of view `view` at head `head`, as [`seed_pending`] seeds canon's.
fn seed_view_pending(e: &Env, push: &str, view: &str, head: Hash, at_ms: i64) -> SyncPending {
    let pending = SyncPending {
        repo_id: repo_of(e),
        url: e.url.clone(),
        push: push.into(),
        kind: KIND_VIEW,
        view: Some(view.into()),
        expected: None,
        head,
        at_ms,
    };
    seed_sync(
        e,
        "shared",
        vec![
            SyncStep::Upload(SyncUpload {
                repo_id: pending.repo_id,
                url: pending.url.clone(),
                push: push.into(),
                kind: KIND_VIEW,
                view: Some(view.into()),
                at_ms,
            }),
            SyncStep::Send(pending.clone()),
        ],
    );
    pending
}

/// The recovery of a found pending write by the next push or pull: the lookup's outcome.
fn recover(s: &Sync<'_>, pull: bool) -> Result<Option<LookupOutcome>, RdyError> {
    if pull {
        s.pull(PullRequest::default())
            .map(|o| o.lookup.map(|l| l.outcome))
    } else {
        s.push(PushRequest::default())
            .map(|o| o.lookup.map(|l| l.outcome))
    }
}

/// How many outcome lookups the hub saw, and the push ids they named.
fn asks(hub: &Hub) -> Vec<String> {
    hub.calls()
        .iter()
        .filter(|c| c.step == "outcome")
        .map(|c| c.push.clone().expect("a lookup names its push"))
        .collect()
}

/// A restore of a project dump whose canon write's reply is lost (the write took), on a hub the
/// `setup` scripts and a fake clock it may use. Returns what the run left.
type Restored = (
    Arc<Hub>,
    Arc<PollClock>,
    tempfile::TempDir,
    Result<RestoreOutcome, RdyError>,
);

fn lost_restore(setup: impl FnOnce(&Arc<Hub>, &Arc<PollClock>)) -> Restored {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let src = env(1);
    let dump = dump_of(&src.engine, dir.path(), "project.rdydump");
    let hub = Arc::new(Hub::new());
    let poll = Arc::new(PollClock::default());
    hub.plan_write(1, WritePlan::Lost { apply: true });
    setup(&hub, &poll);
    let renderer = Renderer::default();
    let result = restore_polled(&caller, &*hub, &renderer, scratch.path(), &dump, &poll);
    (hub, poll, scratch, result)
}

/// A restore of a hub dump that holds view `v1`, whose view write's reply is lost (the write
/// took) after a canon write that is answered.
fn lost_view_restore(setup: impl FnOnce(&Arc<Hub>, &Arc<PollClock>)) -> Restored {
    let dir = private_dir();
    let scratch = private_dir();
    let caller = env(0);
    let hsrc = env(1);
    write_in(&hsrc.engine, "v1", "a");
    let dump = hub_dump(&hsrc, &["v1"], None, dir.path(), "hub.rdydump");
    let hub = Arc::new(Hub::new().with_state(|s| s.repo_id = Some(repo_of(&hsrc))));
    let poll = Arc::new(PollClock::default());
    hub.plan_write(2, WritePlan::Lost { apply: true });
    setup(&hub, &poll);
    let renderer = Renderer::default();
    let result = restore_polled(&caller, &*hub, &renderer, scratch.path(), &dump, &poll);
    (hub, poll, scratch, result)
}

/// The sync words of an uncertain pending write of push `push`.
#[track_caller]
fn assert_sync_uncertain(x: &RdyError, push: &str) {
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            &*format!("the sync write of push {push} may have completed"),
            &*format!(
                "run rdy push or rdy pull again: rdy finds the outcome of push {push} before it writes again"
            ),
        )
    );
}

/// A restore's uncertain words, with the one issue that names `push` and no warning.
#[track_caller]
fn assert_restore_uncertain(x: &RdyError, push: &str) {
    assert_eq!(
        (x.code, x.message.as_str(), x.fix.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            "the hub request may have completed",
            "check its state on the hub before trying again"
        )
    );
    assert_eq!(x.issues.len(), 1, "{:?}", x.issues);
    assert_eq!(
        (x.issues[0].code, x.issues[0].message.as_str()),
        (
            E_UPSTREAM_UNCERTAIN,
            &*format!("the restore's write of push {push} has no known outcome")
        )
    );
    assert_eq!(
        x.issues[0].fix,
        "check its state on the hub before trying again"
    );
}

/// D-GS59E, D-H0BZN: a lookup that finds the hub still running the write waits its
/// `retry_after` and asks again for the same push id, on a pending write's recovery by push and
/// by pull (canon's and a view's) and on a restore's lost write (canon's and a view's). The
/// taken reply applies once, and `pending` clears only then.
#[test]
fn r59016_busy_then_taken() {
    // a pending canon write, found by push and by pull
    for pull in [false, true] {
        let e = env(1);
        let head = e.landings[0];
        seed_pending(&e, P1, head, T0);
        let hub = Hub::new().with_state(|s| s.canon = Some(head));
        *hub.held.lock().unwrap() = closure(&e, head, None);
        let copy = pull.then(|| copy_project(&e));
        if let Some((_, hub_store)) = &copy {
            hub.serve(hub_store.read());
        }
        hub.fail_at("outcome", 1, busy_refusal(2));
        hub.fail_at("outcome", 2, busy_refusal(5));
        hub.answer(P1, LookupOutcome::Taken { tip: landing(head) });
        let s = Sync::new(&e, &hub);
        let out = recover(&s, pull).unwrap();
        assert!(matches!(out, Some(LookupOutcome::Taken { .. })), "{out:?}");
        assert_eq!(asks(&hub), [P1, P1, P1], "pull: {pull}");
        assert_eq!(s.poll.seconds(), [2, 5]);
        assert!(hub.writes().is_empty());
        assert_eq!(pending_of(&e, "shared"), None);
        assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(head));
    }

    // a pending view write: a view op is its taken tip
    let e = env(1);
    write_in(&e.engine, "v1", "a");
    let v1 = view_head(&e, "v1");
    seed_view_pending(&e, P1, "v1", v1, T0);
    let canon = e.landings[0];
    let hub = Hub::new().with_state(|s| s.canon = Some(canon));
    *hub.held.lock().unwrap() = closure(&e, canon, None);
    hub.fail_at("outcome", 1, busy_refusal(2));
    hub.fail_at("outcome", 2, busy_refusal(5));
    hub.answer(
        P1,
        LookupOutcome::Taken {
            tip: ObjectKey {
                kind: kinds::VIEW_OP,
                hash: v1,
            },
        },
    );
    let s = Sync::new(&e, &hub);
    let out = recover(&s, false).unwrap();
    assert!(matches!(out, Some(LookupOutcome::Taken { .. })), "{out:?}");
    assert_eq!(asks(&hub), [P1, P1, P1]);
    assert_eq!(s.poll.seconds(), [2, 5]);
    assert_eq!(pending_of(&e, "shared"), None);
    let applied = e.engine.read().reader().sync_view("shared", "v1").unwrap();
    assert_eq!(applied.unwrap().applied, Some(v1));

    // a restore's lost canon write: one lookup record, the temporary store gone
    let (hub, poll, scratch, out) = lost_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(2));
        hub.fail_at("outcome", 2, busy_refusal(5));
        answer_last_write(hub, Last::Taken);
    });
    let out = out.unwrap();
    assert_eq!(hub.writes().len(), 1);
    let push = hub.writes()[0].push.clone();
    assert_eq!(asks(&hub), [push.clone(), push.clone(), push.clone()]);
    assert_eq!(poll.seconds(), [2, 5]);
    assert_eq!(out.lookups.len(), 1);
    assert_eq!(out.lookups[0].push, push);
    assert!(matches!(
        out.lookups[0].outcome,
        LookupOutcome::Taken { .. }
    ));
    assert_eq!(
        taken_end(&out.canon).2,
        hub.state.lock().unwrap().canon.unwrap()
    );
    assert!(is_empty_dir(scratch.path()));

    // a restore's lost view write: the taken tip must be a view op
    let (hub, poll, scratch, out) = lost_view_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(2));
        hub.fail_at("outcome", 2, busy_refusal(5));
        answer_last_write(hub, Last::Taken);
    });
    let out = out.unwrap();
    assert_eq!(hub.writes().len(), 2);
    let push = hub.writes()[1].push.clone();
    assert_eq!(asks(&hub), [push.clone(), push.clone(), push.clone()]);
    assert_eq!(poll.seconds(), [2, 5]);
    assert_eq!(out.lookups.len(), 1);
    assert_eq!(out.lookups[0].view.as_deref(), Some("v1"));
    assert!(matches!(&out.views[..], [(n, WriteEnd::Taken { .. })] if n == "v1"));
    assert!(is_empty_dir(scratch.path()));

    let (hub, _, scratch, out) = lost_view_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(1));
        answer_last_write(hub, Last::TakenAs(kinds::LANDING));
    });
    assert_eq!(err(out).code, E_UPSTREAM_PROTOCOL);
    assert_eq!(hub.writes().len(), 2);
    assert!(is_empty_dir(scratch.path()));
}

/// D-H0BZN, D-GS59E: the hub state a push or pull captured while the write still ran is stale
/// once the recovery learns the write took. Local canon and the pending head are B (parent A);
/// the pull's state names canon A, the lookup polls busy, then answers taken(B). The operation
/// reads the hub state again after the recovery, so local canon stays B, no landing is marked
/// replaced, and `pending` clears once.
#[test]
fn r59016_busy_then_taken_keeps_the_newer_canon() {
    for pull in [false, true] {
        let e = env(2);
        let (a, b) = (e.landings[0], e.landings[1]);
        seed_pending(&e, P1, b, T0);
        let hub = Arc::new(Hub::new().with_state(|s| s.canon = Some(a)));
        *hub.held.lock().unwrap() = closure(&e, b, None);
        let copy = copy_project(&e);
        hub.serve(copy.1.read());
        hub.fail_at("outcome", 1, busy_refusal(1));
        hub.answer(P1, LookupOutcome::Taken { tip: landing(b) });
        // the write completes while the lookup waits: later state reads name canon B
        let weak = Arc::downgrade(&hub);
        let mut seen = 0;
        *hub.each.lock().unwrap() = Some((
            "outcome",
            Box::new(move |_| {
                seen += 1;
                if seen == 2 {
                    let h = weak.upgrade().expect("the hub is alive");
                    h.state.lock().unwrap().canon = Some(b);
                }
            }),
        ));
        let s = Sync::new(&e, &hub);
        let replaced = if pull {
            let out = s.pull(PullRequest::default()).unwrap();
            assert!(matches!(
                out.lookup.map(|l| l.outcome),
                Some(LookupOutcome::Taken { .. })
            ));
            out.canon.and_then(|c| c.replaced)
        } else {
            let out = s.push(PushRequest::default()).unwrap();
            assert!(matches!(
                out.lookup.map(|l| l.outcome),
                Some(LookupOutcome::Taken { .. })
            ));
            None
        };
        assert_eq!(replaced, None, "pull: {pull}");
        assert_eq!(e.engine.read().canon_head(), Some(b), "pull: {pull}");
        assert_eq!(asks(&hub), [P1, P1], "pull: {pull}");
        assert_eq!(s.poll.seconds(), [1]);
        // the state is read again after the recovery, before any further sync work
        assert_eq!(hub.states.lock().unwrap().len(), 2, "pull: {pull}");
        assert!(hub.writes().is_empty());
        assert_eq!(pending_of(&e, "shared"), None);
        assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(b));
        let marks = e
            .engine
            .read()
            .reader()
            .refs_with_prefix("replaced/")
            .unwrap();
        assert!(marks.is_empty(), "pull: {pull}");
    }

    // a recovery that waited on no busy reply keeps its single state read
    let e = env(2);
    let b = e.landings[1];
    seed_pending(&e, P1, b, T0);
    let hub = Hub::new().with_state(|s| s.canon = Some(b));
    *hub.held.lock().unwrap() = closure(&e, b, None);
    hub.answer(P1, LookupOutcome::Taken { tip: landing(b) });
    let s = Sync::new(&e, &hub);
    s.push(PushRequest::default()).unwrap();
    assert_eq!(hub.states.lock().unwrap().len(), 1);
    assert!(s.poll.seconds().is_empty());
}

/// One recovery of a pending write that the hub keeps busy: returns the lookups made and the
/// seconds waited, after pinning the exact error and that nothing else changed.
fn bounded_pending(view: bool, pull: bool, retry: u64, cost: u64, epoch: i64) -> (usize, Vec<u64>) {
    let e = env(1);
    let head = e.landings[0];
    let sent = if view {
        write_in(&e.engine, "v1", "a");
        let v1 = view_head(&e, "v1");
        seed_view_pending(&e, P1, "v1", v1, T0)
    } else {
        seed_pending(&e, P1, head, T0)
    };
    let hub = Hub::new().with_state(|s| {
        s.canon = Some(head);
        s.quarantine_seconds = QUARANTINE_S;
    });
    script_busy(&hub, retry, 200);
    let s = Sync::new(&e, &hub);
    s.clock.0.store(epoch, Ordering::SeqCst);
    if cost > 0 {
        lookup_costs(&hub, &s.poll, cost);
    }
    let x = err(recover(&s, pull));
    assert_sync_uncertain(&x, P1);
    let n = asks(&hub).len();
    // discovery and state, then lookups only: no known, put, get or write
    assert_eq!(hub.steps().len(), 2 + n, "{:?}", hub.steps());
    assert!(hub.writes().is_empty());
    assert_eq!(pending_of(&e, "shared"), Some(sent));
    assert_eq!(canon_of(&e, "shared"), None);
    assert_eq!(
        e.engine.read().reader().sync_view("shared", "v1").unwrap(),
        None
    );
    (n, s.poll.seconds())
}

/// D-GS59E: past the polling bound the lookup stops with the uncertain words and changes
/// nothing. At zero cost and `retry_after` 1 it makes 82 lookups and 81 waits; at 5, 17 and 16;
/// a lookup that takes 39 seconds leaves room for three lookups and two waits. The bound is the
/// fake monotonic clock's: the epoch clock the stored refs use never feeds it.
#[test]
fn r59016_busy_past_bound_is_uncertain() {
    let cases: [(u64, u64, usize, usize); 3] = [(1, 0, 82, 81), (5, 0, 17, 16), (1, 39, 3, 2)];
    for (retry, cost, calls, waits) in cases {
        let ones = vec![retry.clamp(1, 5); waits];
        for (i, (view, pull)) in [(false, false), (false, true), (true, false)]
            .into_iter()
            .enumerate()
        {
            // the epoch clock may stand anywhere, and go back
            let epoch = [T0, T0 + 2 * QUARANTINE_MS, T0 - 5_000_000][i];
            let (n, seconds) = bounded_pending(view, pull, retry, cost, epoch);
            assert_eq!(
                (n, seconds.clone()),
                (calls, ones.clone()),
                "pending view {view} pull {pull} retry {retry} cost {cost}"
            );
        }

        // a restore's lost write: its own uncertain words, no busy warning
        let (hub, poll, scratch, out) = lost_restore(|hub, poll| {
            script_busy(hub, retry, 200);
            if cost > 0 {
                lookup_costs(hub, poll, cost);
            }
        });
        let x = err(out);
        let push = hub.writes()[0].push.clone();
        assert_restore_uncertain(&x, &push);
        assert_eq!(asks(&hub).len(), calls, "restore retry {retry} cost {cost}");
        assert_eq!(poll.seconds(), ones);
        assert_eq!(hub.writes().len(), 1);
        assert!(is_empty_dir(scratch.path()));

        // a restore's lost view write
        let (hub, poll, scratch, out) = lost_view_restore(|hub, poll| {
            script_busy(hub, retry, 200);
            if cost > 0 {
                lookup_costs(hub, poll, cost);
            }
        });
        let x = err(out);
        let push = hub.writes()[1].push.clone();
        assert_restore_uncertain(&x, &push);
        assert_eq!(asks(&hub).len(), calls);
        assert_eq!(poll.seconds(), ones);
        assert_eq!(hub.writes().len(), 2);
        assert!(is_empty_dir(scratch.path()));
    }
}

/// D-H0BZN "After a lost write reply, the client never sends that write again": while the hub
/// answers busy, the restore and the next push or pull send no known, put, write or get, also
/// when the pending record is older than the quarantine life; a bound exit keeps it so, and a
/// later taken reply clears `pending` without sending that push id's write again.
#[test]
fn r59016_busy_never_sends_a_second_write() {
    // a restore: the one write is the lost one
    let seen = Arc::new(Mutex::new(Vec::<(usize, &'static str)>::new()));
    let (hub, poll, _scratch, out) = lost_restore(|hub, poll| {
        script_busy(hub, 1, 200);
        let (weak, probe) = (Arc::downgrade(hub), Arc::clone(&seen));
        poll.on_wait(move |_| {
            let h = weak.upgrade().unwrap();
            probe
                .lock()
                .unwrap()
                .push((h.writes().len(), *h.steps().last().unwrap()));
        });
    });
    assert_restore_uncertain(&err(out), &hub.writes()[0].push);
    assert_eq!(poll.seconds().len(), 81);
    assert_eq!(seen.lock().unwrap().len(), 81);
    assert!(seen.lock().unwrap().iter().all(|s| *s == (1, "outcome")));
    assert_eq!(hub.writes().len(), 1);
    let steps = hub.steps();
    let write_at = steps.iter().position(|s| *s == "write").unwrap();
    assert!(steps[write_at + 1..].iter().all(|s| *s == "outcome"));

    // a push that loses its write reply stops at once; the next push and pull wait
    let e = env(1);
    let head = e.landings[0];
    let hub = Arc::new(Hub::new().with_state(|s| s.quarantine_seconds = QUARANTINE_S));
    hub.plan_write(1, WritePlan::Lost { apply: true });
    let s = Sync::new(&e, &hub);
    let first = err(s.push(PushRequest::default()));
    let pending = pending_of(&e, "shared").expect("the lost write stays pending");
    assert_sync_uncertain(&first, &pending.push);
    assert!(asks(&hub).is_empty());
    assert!(s.poll.seconds().is_empty());
    assert_eq!(hub.writes().len(), 1);

    // older than the quarantine life, still busy: not cleared, nothing sent
    s.clock.0.store(T0 + 2 * QUARANTINE_MS, Ordering::SeqCst);
    let during = Arc::new(Mutex::new(Vec::<(usize, &'static str)>::new()));
    let (weak, probe) = (Arc::downgrade(&hub), Arc::clone(&during));
    s.poll.on_wait(move |_| {
        let h = weak.upgrade().unwrap();
        probe
            .lock()
            .unwrap()
            .push((h.writes().len(), *h.steps().last().unwrap()));
    });
    let before = hub.steps().len();
    script_busy(&hub, 1, 200);
    for pull in [false, true] {
        assert_sync_uncertain(&err(recover(&s, pull)), &pending.push);
        assert_eq!(pending_of(&e, "shared"), Some(pending.clone()));
        assert_eq!(hub.writes().len(), 1);
    }
    assert_eq!(s.poll.seconds().len(), 162);
    assert!(during.lock().unwrap().iter().all(|s| *s == (1, "outcome")));
    assert!(
        hub.steps()[before..]
            .iter()
            .all(|s| ["discovery", "state", "outcome"].contains(s))
    );

    // taken at last: pending clears, and the write of that push id is not sent again
    hub.fail.lock().unwrap().clear();
    hub.answer(&pending.push, LookupOutcome::Taken { tip: landing(head) });
    let out = s.push(PushRequest::default()).unwrap();
    assert!(matches!(
        out.lookup.map(|l| l.outcome),
        Some(LookupOutcome::Taken { .. })
    ));
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(canon_of(&e, "shared").unwrap().applied, Some(head));
    assert_eq!(hub.writes().len(), 1);
}

/// D-H0BZN, D-H67EN: a busy reply that ends in held or refused is that outcome, applied once.
/// A completed refusal whose code is `E_HUB_BUSY` is not a running write: no wait follows it,
/// whatever the pending record's age. Unknown and every other lookup failure stop at once with
/// their own rule, and the held-request refresh asks once.
#[test]
fn r59016_busy_held_and_refused_are_terminal() {
    // busy, then held: recorded once, no write
    let e = env(1);
    let head = e.landings[0];
    seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.fail_at("outcome", 1, busy_refusal(1));
    hub.answer(
        P1,
        LookupOutcome::Held {
            request: HubRequest {
                number: 4,
                version: 1,
            },
            tip: None,
        },
    );
    let s = Sync::new(&e, &hub);
    let out = s.push(PushRequest::default()).unwrap();
    assert!(matches!(out.canon, WriteEnd::HeldOpen { request: 4, .. }));
    assert_eq!(asks(&hub).len(), 2);
    assert_eq!(s.poll.seconds(), [1]);
    assert_eq!(pending_of(&e, "shared"), None);
    assert_eq!(
        canon_of(&e, "shared").unwrap().held,
        Some(SyncHeld {
            head,
            request: 4,
            push: P1.into(),
        })
    );

    // busy, then refused: cleared once, and the push goes on under a new push id
    let e = env(1);
    let head = e.landings[0];
    seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.fail_at("outcome", 1, busy_refusal(1));
    hub.answer(
        P1,
        LookupOutcome::Refused {
            code: "E_HUB_BEHIND".into(),
        },
    );
    let s = Sync::new(&e, &hub);
    let out = s.push(PushRequest::default()).unwrap();
    assert_eq!(
        hub.steps(),
        ["discovery", "state", "outcome", "outcome", "put", "write"]
    );
    assert_eq!(s.poll.seconds(), [1]);
    assert_ne!(taken(&out).0, P1);
    assert_eq!(pending_of(&e, "shared"), None);

    // a completed refusal with the code E_HUB_BUSY: no wait, however old the record
    for pull in [false, true] {
        let e = env(1);
        let head = e.landings[0];
        seed_pending(&e, P1, head, T0);
        let hub = Hub::new().with_state(|s| s.quarantine_seconds = QUARANTINE_S);
        hub.answer(
            P1,
            LookupOutcome::Refused {
                code: "E_HUB_BUSY".into(),
            },
        );
        let copy = pull.then(|| copy_project(&e));
        if let Some((_, hub_store)) = &copy {
            hub.serve(hub_store.read());
        }
        let s = Sync::new(&e, &hub);
        s.clock.0.store(T0 + 2 * QUARANTINE_MS, Ordering::SeqCst);
        let out = if pull {
            s.pull(PullRequest::default()).map(|_| None)
        } else {
            s.push(PushRequest::default()).map(Some)
        }
        .unwrap();
        assert_eq!(asks(&hub), [P1], "pull {pull}");
        assert!(s.poll.seconds().is_empty());
        assert_eq!(pending_of(&e, "shared"), None);
        if let Some(out) = out {
            // the same behavior as before: a new push id sends the history again
            assert_eq!(
                hub.steps(),
                ["discovery", "state", "outcome", "put", "write"]
            );
            assert_ne!(taken(&out).0, P1);
        }
    }

    // unknown: a young record stops uncertain, at once or after busy; an old one clears
    for (busy, young) in [(false, true), (true, true), (true, false)] {
        let e = env(1);
        let head = e.landings[0];
        let sent = seed_pending(&e, P1, head, T0);
        let hub = Hub::new().with_state(|s| s.quarantine_seconds = QUARANTINE_S);
        if busy {
            hub.fail_at("outcome", 1, busy_refusal(1));
        }
        let s = Sync::new(&e, &hub);
        if !young {
            s.clock.0.store(T0 + 2 * QUARANTINE_MS, Ordering::SeqCst);
        }
        let waits = usize::from(busy);
        let r = s.push(PushRequest::default());
        assert_eq!(s.poll.seconds().len(), waits);
        assert_eq!(asks(&hub).len(), 1 + waits);
        if young {
            assert_sync_uncertain(&err(r), P1);
            assert_eq!(pending_of(&e, "shared"), Some(sent));
        } else {
            assert_eq!(r.unwrap().lookup.unwrap().outcome, LookupOutcome::Unknown);
            assert_eq!(pending_of(&e, "shared"), None);
        }
    }

    // other failures stop at once: after a wait, a hub error of another code; a busy refusal
    // with no wait stays the protocol error
    let e = env(1);
    let head = e.landings[0];
    let sent = seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.fail_at("outcome", 1, busy_refusal(1));
    hub.fail_at("outcome", 2, hub_words("E_HUB_NOT_FOUND"));
    let s = Sync::new(&e, &hub);
    assert_eq!(err(s.push(PushRequest::default())).code, "E_HUB_NOT_FOUND");
    assert_eq!(s.poll.seconds(), [1]);
    assert_eq!(asks(&hub).len(), 2);
    assert_eq!(pending_of(&e, "shared"), Some(sent.clone()));
    let hub = Hub::new();
    hub.fail_at("outcome", 1, transport_failure());
    let s = Sync::new(&e, &hub);
    assert_eq!(
        err(s.push(PushRequest::default())).code,
        E_UPSTREAM_TRANSPORT
    );
    assert!(s.poll.seconds().is_empty());
    let hub = Hub::new();
    hub.fail_at("outcome", 1, hub_words("E_HUB_BUSY"));
    let s = Sync::new(&e, &hub);
    assert_eq!(
        err(s.push(PushRequest::default())).code,
        E_UPSTREAM_PROTOCOL
    );
    assert!(s.poll.seconds().is_empty());
    assert_eq!(asks(&hub).len(), 1);
    assert_eq!(pending_of(&e, "shared"), Some(sent));

    // the held-request refresh asks once and warns: it never waits
    let e = env(2);
    let tip = e.landings[0];
    let hub = Hub::new().with_state(|s| s.canon = Some(tip));
    hub.plan_write(
        1,
        WritePlan::Held {
            number: 3,
            version: 1,
        },
    );
    let out = push(&e, &hub, None).unwrap();
    let WriteEnd::Held { push: held, .. } = &out.canon else {
        panic!("{:?}", out.canon)
    };
    let held = held.clone();
    let (_dir, hub_store) = copy_project(&e);
    hub.serve(hub_store.read());
    hub.fail_at("outcome", 1, busy_refusal(1));
    let s = Sync::new(&e, &hub);
    let out = s.pull(PullRequest::default()).unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|w| w.code == "W_UPSTREAM_HELD_LOOKUP"),
        "{:?}",
        out.warnings
    );
    assert_eq!(asks(&hub), [held]);
    assert!(s.poll.seconds().is_empty());

    // a restore: busy then held, busy then refused, a completed E_HUB_BUSY refusal (no wait),
    // unknown, and a failed lookup after a wait
    let (hub, poll, scratch, out) = lost_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(1));
        answer_last_write(hub, Last::Held(7));
    });
    let out = out.unwrap();
    assert!(
        matches!(&out.canon, Some(WriteEnd::Held { request, .. }) if request.number == 7),
        "{:?}",
        out.canon
    );
    assert_eq!((out.lookups.len(), poll.seconds()), (1, vec![1]));
    assert_eq!(hub.writes().len(), 1);
    assert!(is_empty_dir(scratch.path()));

    for (busy, code, waits) in [(true, "E_HUB_BEHIND", 1), (false, "E_HUB_BUSY", 0)] {
        let (hub, poll, scratch, out) = lost_restore(|hub, _| {
            if busy {
                hub.fail_at("outcome", 1, busy_refusal(1));
            }
            answer_last_write(hub, Last::Refused(code));
        });
        let out = out.unwrap();
        let push = hub.writes()[0].push.clone();
        assert!(
            matches!(&out.canon, Some(WriteEnd::Refused { push: Some(p), .. }) if *p == push),
            "{:?}",
            out.canon
        );
        assert_eq!(out.lookups.len(), 1);
        assert_eq!(poll.seconds().len(), waits);
        assert_eq!(asks(&hub).len(), 1 + waits);
        assert_eq!(hub.writes().len(), 1);
        assert!(is_empty_dir(scratch.path()));
    }

    let (hub, poll, _scratch, out) = lost_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(1));
    });
    assert_restore_uncertain(&err(out), &hub.writes()[0].push);
    assert_eq!((asks(&hub).len(), poll.seconds()), (2, vec![1]));

    let (hub, poll, _scratch, out) = lost_restore(|hub, _| {
        hub.fail_at("outcome", 1, busy_refusal(1));
        hub.fail_at("outcome", 2, hub_words("E_HUB_NOT_FOUND"));
    });
    let x = err(out);
    assert_eq!(x.code, E_UPSTREAM_UNCERTAIN);
    assert_eq!(x.issues.len(), 2, "{:?}", x.issues);
    assert_eq!(x.issues[1].code, "E_HUB_NOT_FOUND");
    assert_eq!((asks(&hub).len(), poll.seconds()), (2, vec![1]));
    assert_eq!(hub.writes().len(), 1);
}

// ---- URL clone: the tip ready first, then the checked history (D-GGTYA, D-4PV25, O16, O17) ----

/// A hub serving `e`'s project with canon at its last landing, `page` keys a known page, and
/// the canon tip's state (stream format 5) when `state`.
fn clone_hub(e: &Env, state: bool, page: usize) -> Hub {
    let hub = Hub::new();
    hub.serve(e.engine.read());
    hub.state.lock().unwrap().canon = Some(*e.landings.last().unwrap());
    *hub.page.lock().unwrap() = page;
    if state {
        hub.send_canon_state();
    }
    hub
}

/// The URL clone of `e`'s hub repo into `dest` over `hub`, returned at Ready.
fn clone_ready(
    e: &Env,
    hub: &Hub,
    dest: &std::path::Path,
) -> Result<rdy_engine::clone::UrlCloneSummary, RdyError> {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    rdy_engine::clone::clone_url_via(
        &writer(),
        &e.url,
        dest,
        &e.user_file,
        &api,
        &clock,
        hub,
        None,
    )
}

/// The foreground continuation of the ready clone `s` over `hub`; `steps` collects each step's
/// progress.
fn continue_clone(
    e: &Env,
    hub: &Hub,
    s: rdy_engine::clone::UrlCloneSummary,
    steps: &mut Vec<rdy_engine::partial_history::HistoryProgress>,
) -> rdy_engine::clone::UrlCloneSummary {
    let api = NoApi::default();
    let clock = Clock::at(T0);
    rdy_engine::clone::continue_history(
        &writer(),
        s,
        &e.user_file,
        &api,
        &clock,
        hub,
        &mut |_| {},
        &mut |p| steps.push(*p),
    )
}

/// Each known pull and get the hub served, in order, with its purpose.
fn wire_order(hub: &Hub) -> Vec<(&'static str, StreamPurpose)> {
    let pulls = hub.pulls.lock().unwrap().clone();
    let gets = hub.gets.lock().unwrap().clone();
    let (mut p, mut g) = (pulls.iter(), gets.iter());
    let mut out = Vec::new();
    for c in hub.calls() {
        match c.step {
            "known_pull" => out.push(("known_pull", p.next().unwrap().purpose)),
            "get" => {
                // a failed get is entered but never recorded
                if let Some(r) = g.next() {
                    out.push(("get", r.purpose));
                }
            }
            _ => {}
        }
    }
    out
}

/// The canon history keys the hub got for `gets`.
fn got_history(gets: &[GetRequest]) -> BTreeSet<ObjectKey> {
    gets.iter()
        .filter(|g| g.purpose == StreamPurpose::CanonHistory)
        .flat_map(|g| g.keys.iter().copied())
        .collect()
}

fn hub_gets(hub: &Hub) -> Vec<GetRequest> {
    hub.gets.lock().unwrap().clone()
}

/// The history ref of the project at `dir`.
fn history_ref(dir: &std::path::Path, remote: &str) -> rdy_store::sync_state::SyncHistory {
    let engine = Local::open(dir).unwrap();
    let h = engine
        .read()
        .reader()
        .sync_history(remote)
        .unwrap()
        .unwrap();
    engine.close().unwrap();
    h
}

/// O16: every canon history key at the captured tip is listed (known reaches its last page)
/// before the state take; the checked list's exact distinct count is Ready's denominator. A
/// resume makes no known call and gets only the keys still missing. The history ref is written
/// twice: at install (complete false) and at completion; no step writes progress.
#[test]
fn r59019_key_list_precedes_ready_and_survives_resume() {
    let e = env(4);
    let hub = clone_hub(&e, true, 2);
    let tip = *e.landings.last().unwrap();
    let want: BTreeSet<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .collect();
    assert!(want.len() > 4, "{}", want.len());
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    // at Ready: the history listing ran to its end, base none at the captured tip, and no
    // history record was got
    let order = wire_order(&hub);
    let history_pulls: Vec<KnownPull> = hub
        .pulls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.purpose == StreamPurpose::CanonHistory)
        .cloned()
        .collect();
    assert_eq!(history_pulls.len(), want.len().div_ceil(2), "{order:?}");
    for p in &history_pulls {
        assert_eq!((p.base, p.root), (None, landing(tip)));
    }
    assert!(
        !order
            .iter()
            .any(|(s, p)| *s == "get" && *p == StreamPurpose::CanonHistory),
        "{order:?}"
    );
    let p = s.history.expect("a ready clone's progress");
    assert_eq!((p.total, p.complete), (want.len() as u64, false));
    assert!(p.held < p.total, "{p:?}");
    assert_eq!(
        s.ready_text().unwrap(),
        format!(
            "ready: work in {} now; history arriving: {} of {} records",
            s.project, p.held, p.total
        )
    );
    assert!(s.notes.is_empty(), "{:?}", s.notes);
    let installed = history_ref(&dest, &s.remote);
    assert_eq!((installed.complete, installed.records), (false, p.total));
    // a continuation that stops at its second get keeps the ready project and warns
    hub.clear();
    hub.gets.lock().unwrap().clear();
    hub.pulls.lock().unwrap().clear();
    // gets of at most two records each; the third fails
    hub.state.lock().unwrap().batch_records = 2;
    hub.fail_at("get", 3, hub_not_found());
    let mut steps = Vec::new();
    let mut stopped = continue_clone(&e, &hub, s, &mut steps);
    assert_eq!(stopped.warnings.len(), 1, "{:?}", stopped.warnings);
    assert_eq!(stopped.warnings[0].code, "E_HUB_NOT_FOUND");
    assert!(
        hub.pulls.lock().unwrap().is_empty(),
        "a resume lists no key"
    );
    let first = got_history(&hub_gets(&hub));
    assert!(!first.is_empty());
    assert_eq!(history_ref(&dest, &stopped.remote), installed);
    // the resume: no known call, only missing keys, then complete
    hub.clear();
    hub.gets.lock().unwrap().clear();
    hub.fail.lock().unwrap().clear();
    stopped.warnings.clear();
    let mut steps = Vec::new();
    let done = continue_clone(&e, &hub, stopped, &mut steps);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert!(
        hub.pulls.lock().unwrap().is_empty(),
        "a resume lists no key"
    );
    let second = got_history(&hub_gets(&hub));
    assert!(first.is_disjoint(&second), "{first:?} {second:?}");
    let all: BTreeSet<ObjectKey> = first.union(&second).copied().collect();
    assert!(all.is_subset(&want));
    let p = done.history.unwrap();
    assert_eq!(
        (p.held, p.total, p.complete),
        (p.total, want.len() as u64, true)
    );
    let completed = history_ref(&dest, &done.remote);
    assert!(completed.complete);
    assert_eq!(
        (completed.frontier, completed.records, completed.repo_id),
        (installed.frontier, installed.records, installed.repo_id)
    );
    // every step's progress came back from the steps, none was stored
    assert!(
        steps.windows(2).all(|w| w[0].held <= w[1].held),
        "{steps:?}"
    );
    let engine = Local::open(&dest).unwrap();
    let read = engine.read();
    for k in &want {
        assert!(read.reader().has_kind(k.kind, &k.hash).unwrap(), "{k:?}");
    }
    assert!(read.require_complete_history().is_ok());
    drop(read);
    engine.close().unwrap();
}

/// A13: a restore entered from a project whose history is still arriving is E_HISTORY_PENDING
/// with the exact message and fix, before any capture, put or write.
#[test]
fn r59019_restore_refuses_while_history_pending() {
    let e = env(3);
    let hub = clone_hub(&e, true, 100);
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let p = s.history.unwrap();
    assert!(!p.complete);
    let dump = dump_of(&e.engine, dir.path(), "full.rdydump");
    let engine = Local::open(&dest).unwrap();
    hub.clear();
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let renderer = Renderer::default();
    let scratch = private_dir();
    let service = SyncService::new(&dest, &e.user_file, &api, &clock, &hub, &renderer)
        .scratch_in(scratch.path());
    let request = || RestoreRequest {
        dump: vec![dump.clone()],
        remote: None,
    };
    for x in [
        err(service.restore_from(&engine, &agent(), request())),
        err(service.restore_preview_from(&engine, &agent(), request())),
    ] {
        assert_eq!(x.code, E_HISTORY_PENDING);
        assert_eq!(
            x.message,
            rdy_engine::partial_history::history_pending_message(p.held, p.total)
        );
        assert_eq!(x.fix, rdy_engine::partial_history::HISTORY_PENDING_FIX);
    }
    assert!(hub.calls().is_empty(), "{:?}", hub.steps());
    assert!(renderer.scopes.lock().unwrap().is_empty());
    engine.close().unwrap();
}

/// D-GGTYA: a ready clone takes reads, writes and landings under their ordinary gates before
/// any history record arrives; the completion keeps that local work.
#[test]
fn r59018_ready_allows_work_before_history() {
    let e = env(3);
    let hub = clone_hub(&e, true, 100);
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    assert!(s.history_pending());
    let engine = Local::open(&dest).unwrap();
    write_in(&engine, "draft", "local-draft");
    let local = land_in(&engine, "local-landed");
    let draft = view_head_of(&engine, "draft");
    engine.close().unwrap();
    let mut steps = Vec::new();
    let done = continue_clone(&e, &hub, s, &mut steps);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert!(done.history.unwrap().complete);
    let engine = Local::open(&dest).unwrap();
    let read = engine.read();
    assert_eq!(read.canon_head(), Some(local));
    drop(read);
    assert_eq!(view_head_of(&engine, "draft"), draft);
    engine.close().unwrap();
}

/// The head of view `name` in `engine`'s project.
fn view_head_of(engine: &Local, name: &str) -> Hash {
    engine.read().view_head(name).unwrap().head
}

/// An older hub (no format 5): the clone takes the whole history before it is ready, with the
/// exact note. A hub that advertises format 5 and sends a bad state batch is an error, never a
/// retry as an older hub, and leaves no project.
#[test]
fn r59018_old_hub_fallback_and_settings_stay_exact() {
    let e = env(3);
    let hub = clone_hub(&e, false, 100);
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    assert_eq!(
        s.notes,
        vec!["this hub sends history only; the clone waits for all of it".to_string()]
    );
    assert!(!s.history_pending());
    assert_eq!(s.ready_text(), None);
    assert!(
        hub.pulls
            .lock()
            .unwrap()
            .iter()
            .all(|p| p.purpose != StreamPurpose::CanonState)
    );
    // a bad format-5 reply
    let bad = clone_hub(&e, true, 100);
    *bad.get_bytes.lock().unwrap() = Some(b"not a batch".to_vec());
    let dest = dir.path().join("bad");
    let x = err(clone_ready(&e, &bad, &dest));
    assert_ne!(x.code, "", "{x:?}");
    let gets = hub_gets(&bad);
    assert_eq!(gets.len(), 1, "{gets:?}");
    assert_eq!(gets[0].purpose, StreamPurpose::CanonState);
    assert!(
        bad.pulls
            .lock()
            .unwrap()
            .iter()
            .all(|p| p.purpose == StreamPurpose::CanonState),
        "no fallback to the history"
    );
    assert!(!dest.join(".rdy").exists());
}

/// One "process" of the history fetch of the clone at `dest`: opens it, begins the fetch and
/// runs steps until one is complete or `stop` says so after it, then drops the fetch and closes
/// the project, as a crash leaves it. Returns the last step and the gets it made.
fn fetch_until(
    e: &Env,
    hub: &Hub,
    dest: &std::path::Path,
    remote: &str,
    stop: &dyn Fn(&rdy_engine::history_fetch::HistoryFetchStep) -> bool,
) -> (rdy_engine::history_fetch::HistoryFetchStep, usize) {
    use rdy_engine::history_fetch::HistoryFetchStep;
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let renderer = Renderer::default();
    let engine = Local::open(dest).unwrap();
    let service = SyncService::new(dest, &e.user_file, &api, &clock, hub, &renderer);
    let before = hub_gets(hub).len();
    let mut fetch = service.begin_history(&engine, &writer(), remote).unwrap();
    let last = loop {
        let st = service
            .resume_history_step(&engine, &writer(), &mut fetch)
            .unwrap();
        if matches!(st, HistoryFetchStep::Complete(_)) || stop(&st) {
            break st;
        }
    };
    drop(fetch);
    engine.close().unwrap();
    (last, hub_gets(hub).len() - before)
}

/// The keys of the staged batch file `path`.
fn staged_keys(path: &std::path::Path) -> Vec<ObjectKey> {
    let bytes = std::fs::read(path).unwrap();
    rdy_store::sync_stream::check_batch(&bytes[..])
        .unwrap()
        .keys
}

/// Whether the project at `dest` holds every key of `keys`.
fn holds_all(dest: &std::path::Path, keys: &BTreeSet<ObjectKey>) -> bool {
    let engine = Local::open(dest).unwrap();
    let read = engine.read();
    let all = keys
        .iter()
        .all(|k| read.reader().has_kind(k.kind, &k.hash).unwrap());
    drop(read);
    engine.close().unwrap();
    all
}

/// D-KE83X: a fetch cut after each checked file keeps the files; the next one gets only the
/// keys no kept file holds. A kept file that changed is refused and its keys got again; a
/// `.part` is removed. A hub that moved on and local landings made meanwhile change nothing of
/// the frontier being completed, and the local work stays.
#[test]
fn r59018_resume_keeps_checked_files_and_local_work() {
    use rdy_engine::history_fetch::{HistoryFetchStep, staged_batches_for_tests};
    let e = env(5);
    let hub = clone_hub(&e, true, 100);
    hub.state.lock().unwrap().batch_records = 2;
    let tip = *e.landings.last().unwrap();
    let want: BTreeSet<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .collect();
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let installed = history_ref(&dest, &s.remote);
    // two cuts, each after one checked file
    let one = |_: &HistoryFetchStep| true;
    for n in 1..=2 {
        let (st, gets) = fetch_until(&e, &hub, &dest, &s.remote, &one);
        assert!(matches!(st, HistoryFetchStep::More(_)), "{st:?}");
        assert_eq!(gets, 1);
        assert_eq!(staged_batches_for_tests(&dest, &s.remote).len(), n);
    }
    let staged = staged_batches_for_tests(&dest, &s.remote);
    let kept = staged_keys(&staged[0]);
    let changed = staged_keys(&staged[1]);
    // the second file changes; a .part is left behind
    let mut bytes = std::fs::read(&staged[1]).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&staged[1], &bytes).unwrap();
    let part = staged[1].with_extension("part");
    std::fs::write(&part, b"half a reply").unwrap();
    // the hub moves on; the clone lands work of its own
    let (_hub_dir, hub_store) = copy_project(&e);
    let later = land_in(&hub_store, "later");
    hub.serve(hub_store.read());
    hub.state.lock().unwrap().canon = Some(later);
    let engine = Local::open(&dest).unwrap();
    let local = land_in(&engine, "local");
    engine.close().unwrap();
    hub.gets.lock().unwrap().clear();
    let (st, _) = fetch_until(&e, &hub, &dest, &s.remote, &|_| false);
    assert!(matches!(st, HistoryFetchStep::Complete(_)), "{st:?}");
    let got = got_history(&hub_gets(&hub));
    assert!(
        kept.iter().all(|k| !got.contains(k)),
        "a kept file is not got again"
    );
    assert!(
        changed.iter().all(|k| got.contains(k)),
        "a changed file is got again"
    );
    assert!(!part.exists());
    assert!(staged_batches_for_tests(&dest, &s.remote).is_empty());
    let done = history_ref(&dest, &s.remote);
    assert!(done.complete);
    assert_eq!(
        (done.frontier, done.records),
        (installed.frontier, installed.records)
    );
    assert!(holds_all(&dest, &want));
    let engine = Local::open(&dest).unwrap();
    assert_eq!(engine.read().canon_head(), Some(local));
    let landed = engine
        .read()
        .reader()
        .has_kind(kinds::LANDING, &later)
        .unwrap();
    assert!(!landed, "the hub's move is the next pull's");
    engine.close().unwrap();
}

/// A cut at each phase: before the take, after the take and before the full check, after the
/// check and before the completion, and after the completion before the cleanup. Each reopen
/// picks the right phase: no get once every key is staged, no second take, and complete stays
/// false until a full check passed.
#[test]
fn r59018_completion_crash_never_claims_missing_history() {
    use rdy_engine::history_fetch::{HistoryFetchStep, staged_batches_for_tests};
    let e = env(4);
    let hub = clone_hub(&e, true, 100);
    hub.state.lock().unwrap().batch_records = 2;
    let tip = *e.landings.last().unwrap();
    let want: BTreeSet<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .collect();
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let total = s.history.unwrap().total;
    let remote = s.remote.clone();
    // every key staged, the take not yet run
    let staged_all = |st: &HistoryFetchStep| st.progress().held == total;
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &staged_all);
    assert!(
        matches!(st, HistoryFetchStep::More(_)) && gets > 1,
        "{st:?}"
    );
    assert!(!holds_all(&dest, &want));
    assert!(!history_ref(&dest, &remote).complete);
    let files = staged_batches_for_tests(&dest, &remote);
    assert!(!files.is_empty());
    let saved = std::fs::read(&files[0]).unwrap();
    let one = |_: &HistoryFetchStep| true;
    // the take, then a cut
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &one);
    assert_eq!(
        (st, gets),
        (
            HistoryFetchStep::More(rdy_engine::partial_history::HistoryProgress {
                frontier: tip,
                complete: false,
                held: total,
                total,
            }),
            0
        )
    );
    assert!(holds_all(&dest, &want));
    assert!(!history_ref(&dest, &remote).complete);
    // the full check, then a cut: still not complete
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &one);
    assert!(matches!(st, HistoryFetchStep::More(_)), "{st:?}");
    assert_eq!(gets, 0);
    assert!(!history_ref(&dest, &remote).complete);
    // a passing check is not stored: the next process checks again before the completion
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &one);
    assert!(matches!(st, HistoryFetchStep::More(_)), "{st:?}");
    assert_eq!(gets, 0);
    assert!(!history_ref(&dest, &remote).complete);
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &|_| false);
    assert!(matches!(st, HistoryFetchStep::Complete(_)), "{st:?}");
    assert_eq!(gets, 0);
    let done = history_ref(&dest, &remote);
    assert!(done.complete);
    assert!(staged_batches_for_tests(&dest, &remote).is_empty());
    // a cut after the completion and before the cleanup: the next fetch only cleans up
    std::fs::write(&files[0], &saved).unwrap();
    let (st, gets) = fetch_until(&e, &hub, &dest, &remote, &one);
    assert!(matches!(st, HistoryFetchStep::Complete(_)), "{st:?}");
    assert_eq!(gets, 0);
    assert!(!files[0].exists());
    assert_eq!(history_ref(&dest, &remote), done);
    let engine = Local::open(&dest).unwrap();
    assert!(engine.read().require_complete_history().is_ok());
    engine.inspect(Rdy::fsck_full).unwrap();
    engine.close().unwrap();
}

/// The sentence the drift projects' two prose blocks share.
const SAID: &str = "Every job retries for five seconds and then reports a failure to the owner.";

/// Writes `ops` in view `work` of `engine`'s project, answers the change plan's open items,
/// proposes and lands it. Returns the landing and the write.
fn land_ops(engine: &Local, ops: serde_json::Value) -> (Hash, WriteResult) {
    rdy_engine::engine::with_writer_result(engine, move |r| {
        let w = r.write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "drift".into(),
            },
        )?;
        common::answer_open_items(r, &writer(), "work");
        let p = r.propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))?;
        common::confirm_exceptions(r, &founder(), &p);
        let l: Hash = r.land(&founder(), &p.id)?.parse().unwrap();
        Ok((l, w))
    })
    .unwrap()
}

/// A project whose canon holds two prose blocks that share [`SAID`], then an edit of the first
/// one that the change plan answers; with `unrelated`, one more landing of an unrelated doc
/// after it, so the edit is not the tip.
fn drift_env(unrelated: bool) -> Env {
    let mut e = env(1);
    let (l, w) = land_ops(
        &e.engine,
        serde_json::json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "jobs", "title": "Jobs", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": SAID},
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "ops", "title": "Ops", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$y", "parent": "$e", "body": format!("In short: {SAID}")}
        ]),
    );
    e.landings.push(l);
    let x = w.created["$x"].clone();
    let (l, _) = land_ops(
        &e.engine,
        serde_json::json!([{"op": "update", "id": x,
            "body": "Every job retries for ten seconds and then reports a failure to the owner."}]),
    );
    e.landings.push(l);
    if unrelated {
        e.landings.push(land_in(&e.engine, "unrelated"));
    }
    e
}

/// The open restatement drift tensions of the project at `dir`, as their summaries.
fn drifts_at(dir: &std::path::Path) -> Vec<String> {
    let engine = Local::open(dir).unwrap();
    let mut out: Vec<String> = engine
        .read()
        .tensions()
        .unwrap()
        .iter()
        .filter(|t| t.kind == rdy_engine::tensions::TensionKind::RestatementDrift)
        .map(|t| t.summary.clone())
        .collect();
    engine.close().unwrap();
    out.sort();
    out
}

/// The logged opening of each open tension of the project at `dir`, as its record's JSON
/// bytes with the record's clock, in tension order.
fn opened_moves(dir: &std::path::Path) -> Vec<(Vec<u8>, i64)> {
    let engine = Local::open(dir).unwrap();
    let read = engine.read();
    let reader = read.reader();
    let mut out = Vec::new();
    for (id, record) in reader.open_tensions().unwrap() {
        let t = reader.transitions(&record).unwrap();
        let m = t.opened.iter().find(|m| m.tension == id).unwrap();
        out.push((serde_json::to_vec(m).unwrap(), t.at_ms));
    }
    drop(read);
    engine.close().unwrap();
    out
}

/// How many TRANSITION records the log of the project at `dir` holds.
fn transitions_logged(dir: &std::path::Path) -> usize {
    let engine = Local::open(dir).unwrap();
    let read = engine.read();
    let mut n = 0;
    read.reader()
        .for_each_object_of(kinds::TRANSITION, |_| {
            n += 1;
            Ok(())
        })
        .unwrap();
    drop(read);
    engine.close().unwrap();
    n
}

/// The sealed batches of the log of the project at `dir`, in log order: each data frame's
/// logical kind and hash, and the ref name and value of each REF_UPDATE frame (the marker left
/// out).
type LogFrame = (u8, Hash, Option<(String, Vec<u8>)>);

fn log_batches(dir: &std::path::Path) -> Vec<Vec<LogFrame>> {
    let engine = Local::open(dir).unwrap();
    let batches = engine
        .inspect(|r| {
            let reader = r.store().reader();
            let mut out = Vec::new();
            let mut cur = Vec::new();
            reader.for_each_frame_from(ORIGIN, r.store().log_end(), |_, raw, h, _, _| {
                let kind = rdy_store::codec::logical_kind(raw);
                if kind == kinds::BATCH_COMMIT {
                    out.push(std::mem::take(&mut cur));
                } else {
                    let r = (kind == kinds::REF_UPDATE).then(|| {
                        let u: kinds::RefUpdate = reader.get_typed(kinds::REF_UPDATE, &h).unwrap();
                        (u.name, u.value)
                    });
                    cur.push((kind, h, r));
                }
                Ok(())
            })?;
            Ok(out)
        })
        .unwrap();
    engine.close().unwrap();
    batches
}

/// The batches of `dir`'s log that write remote `remote`'s history ref as complete.
fn completion_batches(dir: &std::path::Path, remote: &str) -> Vec<Vec<LogFrame>> {
    let name = rdy_store::sync_state::history_ref(remote);
    log_batches(dir)
        .into_iter()
        .filter(|b| {
            b.iter().any(|(_, _, r)| {
                r.as_ref().is_some_and(|(n, v)| {
                    *n == name
                        && matches!(
                            rdy_store::sync_state::check_ref(n, v),
                            Ok(Some(rdy_store::sync_state::SyncValue::History(h))) if h.complete
                        )
                })
            })
        })
        .collect()
}

/// The TRANSITION record that logged the opening of each open tension of the project at `dir`.
fn opening_records(dir: &std::path::Path) -> Vec<Hash> {
    let engine = Local::open(dir).unwrap();
    let out = engine
        .read()
        .reader()
        .open_tensions()
        .unwrap()
        .into_iter()
        .map(|(_, record)| record)
        .collect();
    engine.close().unwrap();
    out
}

/// A copy of the directory tree `from` at `to` (a closed project).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&p, to.join(entry.file_name())).unwrap();
        }
    }
}

/// The history fetch of `remote` in the project at `dest` over `hub`, driven past its take and
/// its full check (every key of `want` held), then `arm` run on the writer's project and the
/// completion step taken. Returns that step and the log's end before it. The engine is dropped
/// with no clean close, as a cut process leaves it.
fn complete_after(
    e: &Env,
    hub: &Hub,
    dest: &std::path::Path,
    remote: &str,
    want: &BTreeSet<ObjectKey>,
    arm: impl FnOnce(&mut Rdy) + Send + 'static,
) -> (
    Result<rdy_engine::history_fetch::HistoryFetchStep, RdyError>,
    rdy_store::segment::Location,
) {
    use rdy_engine::history_fetch::HistoryFetchStep;
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let renderer = Renderer::default();
    let engine = Local::open(dest).unwrap();
    let service = SyncService::new(dest, &e.user_file, &api, &clock, hub, &renderer);
    let mut fetch = service.begin_history(&engine, &writer(), remote).unwrap();
    let taken = |engine: &Local| {
        let read = engine.read();
        want.iter()
            .all(|k| read.reader().has_kind(k.kind, &k.hash).unwrap())
    };
    while !taken(&engine) {
        let st = service
            .resume_history_step(&engine, &writer(), &mut fetch)
            .unwrap();
        assert!(matches!(st, HistoryFetchStep::More(_)), "{st:?}");
    }
    // the full check
    let st = service
        .resume_history_step(&engine, &writer(), &mut fetch)
        .unwrap();
    assert!(matches!(st, HistoryFetchStep::More(_)), "{st:?}");
    assert!(!history_ref_in(&engine, remote).complete);
    let end = engine.inspect(|r| Ok(r.store().log_end())).unwrap();
    rdy_engine::engine::with_writer_result(&engine, move |r| {
        arm(r);
        Ok(())
    })
    .unwrap();
    let st = service.resume_history_step(&engine, &writer(), &mut fetch);
    drop(fetch);
    drop(engine);
    (st, end)
}

/// The history ref of `remote` in `engine`'s project.
fn history_ref_in(engine: &Local, remote: &str) -> rdy_store::sync_state::SyncHistory {
    engine
        .read()
        .reader()
        .sync_history(remote)
        .unwrap()
        .unwrap()
}

/// D-GGTYA: while the history ref is incomplete, canon's rows stop at the frontier, so the
/// older landings that a completion's take holds are not canon's yet. A fresh read after the
/// take (a reopen before the completion) reads no tension of theirs, as before the take, and so
/// logs none that the completion would re-key; the completion then opens the reference's drift.
#[test]
fn r59019_taken_history_stays_behind_the_frontier_until_complete() {
    let e = drift_env(false);
    let dir = private_dir();
    let reference = dir.path().join("reference");
    let full = clone_hub(&e, false, 100);
    clone_ready(&e, &full, &reference).unwrap();
    let want_drifts = drifts_at(&reference);
    assert_eq!(want_drifts.len(), 1, "{want_drifts:?}");
    let hub = clone_hub(&e, true, 100);
    let tip = *e.landings.last().unwrap();
    let want: BTreeSet<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .collect();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let total = s.history.unwrap().total;
    let remote = s.remote.clone();
    let before = drifts_at(&dest);
    assert!(before.is_empty(), "{before:?}");
    // every key staged, then the take
    let staged_all = |st: &rdy_engine::history_fetch::HistoryFetchStep| st.progress().held == total;
    fetch_until(&e, &hub, &dest, &remote, &staged_all);
    fetch_until(&e, &hub, &dest, &remote, &|_| true);
    assert!(holds_all(&dest, &want));
    assert!(!history_ref(&dest, &remote).complete);
    assert_eq!(drifts_at(&dest), before);
    // a write in a fresh process logs no move of the older landings
    let engine = Local::open(&dest).unwrap();
    land_in(&engine, "local");
    engine.close().unwrap();
    assert_eq!(transitions_logged(&dest), 0);
    let (st, _) = fetch_until(&e, &hub, &dest, &remote, &|_| false);
    assert!(
        matches!(st, rdy_engine::history_fetch::HistoryFetchStep::Complete(_)),
        "{st:?}"
    );
    assert_eq!(drifts_at(&dest), want_drifts);
    assert_eq!(transitions_logged(&dest), 1);
}

/// D-GGTYA: the completion's moves are durable with the completed history ref, in its one
/// batch. A cut at the append of the moves' TRANSITION frame (the frame a completion that
/// published its moves in a later batch would write after its ref was durable) leaves the
/// history incomplete with no move logged; the next completion logs the reference's move once,
/// under cause recheck, in the batch that completes the ref, and a reindex keeps it.
#[test]
fn r59019_completion_cut_before_its_moves_never_completes_without_them() {
    let e = drift_env(false);
    let dir = private_dir();
    let reference = dir.path().join("reference");
    let full = clone_hub(&e, false, 100);
    clone_ready(&e, &full, &reference).unwrap();
    let want_drifts = drifts_at(&reference);
    assert_eq!(want_drifts.len(), 1, "{want_drifts:?}");
    let as_recheck = |m: &[u8]| {
        let mut t: rdy_store::transition::TensionMove = serde_json::from_slice(m).unwrap();
        t.why.cause = rdy_store::transition::Cause::Recheck;
        serde_json::to_vec(&t).unwrap()
    };
    let reference_moves: Vec<Vec<u8>> = opened_moves(&reference)
        .iter()
        .map(|(m, _)| as_recheck(m))
        .collect();
    let hub = clone_hub(&e, true, 100);
    let tip = *e.landings.last().unwrap();
    let want: BTreeSet<ObjectKey> = hub
        .history(StreamPurpose::CanonHistory, tip, None)
        .into_iter()
        .collect();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let remote = s.remote.clone();
    let engine = Local::open(&dest).unwrap();
    land_in(&engine, "local");
    engine.close().unwrap();
    // a dry run on a copy finds the frame the moves' TRANSITION takes after the completed ref
    let dry = dir.path().join("dry");
    copy_tree(&dest, &dry);
    let (st, end) = complete_after(&e, &hub, &dry, &remote, &want, |_| {});
    assert!(
        matches!(
            st,
            Ok(rdy_engine::history_fetch::HistoryFetchStep::Complete(_))
        ),
        "{st:?}"
    );
    let engine = Local::open(&dry).unwrap();
    let mut seen = Vec::new();
    engine
        .inspect(|r| {
            r.store().reader().for_each_frame_from(
                end,
                r.store().log_end(),
                |_, raw, _, _, _| {
                    seen.push(rdy_store::codec::logical_kind(raw));
                    Ok(())
                },
            )?;
            Ok(())
        })
        .unwrap();
    engine.close().unwrap();
    let completed = seen
        .iter()
        .position(|k| *k == kinds::REF_UPDATE)
        .expect("the completion writes its ref");
    let cut = completed
        + seen[completed..]
            .iter()
            .position(|k| *k == kinds::TRANSITION)
            .expect("the completion logs its moves");
    // the cut: every frame before the moves' TRANSITION is written, that one fails
    let (st, _) = complete_after(&e, &hub, &dest, &remote, &want, move |r| {
        r.store_mut().fail_append_at_frame_for_tests(Some((cut, 0)));
    });
    assert!(st.is_err(), "{st:?}");
    let after_cut = history_ref(&dest, &remote);
    assert!(
        !after_cut.complete,
        "a completed history without its moves: {:?}",
        opened_moves(&dest)
    );
    assert!(completion_batches(&dest, &remote).is_empty());
    assert_eq!(transitions_logged(&dest), 0);
    // the next completion logs the reference's move once, in the batch that completes the ref
    let (st, _) = fetch_until(&e, &hub, &dest, &remote, &|_| false);
    assert!(
        matches!(st, rdy_engine::history_fetch::HistoryFetchStep::Complete(_)),
        "{st:?}"
    );
    assert_eq!(drifts_at(&dest), want_drifts);
    let moves = opened_moves(&dest);
    assert_eq!(
        moves.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
        reference_moves
    );
    let batches = completion_batches(&dest, &remote);
    assert_eq!(batches.len(), 1, "{batches:?}");
    let opened = opening_records(&dest);
    assert_eq!(opened.len(), 1);
    assert!(
        batches[0]
            .iter()
            .any(|(k, h, _)| *k == kinds::TRANSITION && *h == opened[0]),
        "{batches:?}"
    );
    let engine = Local::open(&dest).unwrap();
    rdy_engine::engine::with_writer_result(&engine, |r| r.reindex()).unwrap();
    engine.close().unwrap();
    assert_eq!(opened_moves(&dest), moves);
}

/// A completion whose take writes the fetched PLAN records of the older landings carries the
/// fetched batch's sync state, though the state take already named that tip: the plans keep
/// the holder's rules and move no `plan/` ref. The full check, the completion and a reindex
/// then pass.
#[test]
fn r59019_completion_takes_older_plans_as_fetched() {
    let e = drift_env(true);
    let hub = clone_hub(&e, true, 100);
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    assert!(s.history_pending());
    let mut steps = Vec::new();
    let done = continue_clone(&e, &hub, s, &mut steps);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert!(done.history.unwrap().complete);
    let engine = Local::open(&dest).unwrap();
    engine.inspect(Rdy::fsck_full).unwrap();
    rdy_engine::engine::with_writer_result(&engine, |r| r.reindex()).unwrap();
    engine.inspect(Rdy::fsck_full).unwrap();
    engine.close().unwrap();
}

/// The older landings a completion makes visible order by canon's ancestry, not by where the
/// log holds them (the state take wrote the tip first): the completed clone keeps the drift
/// the complete-history reference has, after a reopen and a reindex too.
#[test]
fn r59019_completion_logs_the_history_moves() {
    let e = drift_env(false);
    let dir = private_dir();
    let reference = dir.path().join("reference");
    let full = clone_hub(&e, false, 100);
    clone_ready(&e, &full, &reference).unwrap();
    let want = drifts_at(&reference);
    assert_eq!(want.len(), 1, "{want:?}");
    let hub = clone_hub(&e, true, 100);
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let s_remote = s.remote.clone();
    // local work before the history arrives: an unrelated landing and a draft
    let engine = Local::open(&dest).unwrap();
    let local = land_in(&engine, "local");
    write_in(&engine, "draft", "local-draft");
    let draft = view_head_of(&engine, "draft");
    engine.close().unwrap();
    let mut steps = Vec::new();
    let done = continue_clone(&e, &hub, s, &mut steps);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert_eq!(drifts_at(&dest), want);
    // the completion moved no canon or view ref
    let engine = Local::open(&dest).unwrap();
    assert_eq!(engine.read().canon_head(), Some(local));
    assert_eq!(view_head_of(&engine, "draft"), draft);
    engine.close().unwrap();
    // the logged opening: the reference's move, logged once under cause recheck
    let moves = opened_moves(&dest);
    // the reference's clone logged its opening as a write; the completion's is a recheck
    let as_recheck = |m: &[u8]| {
        let mut t: rdy_store::transition::TensionMove = serde_json::from_slice(m).unwrap();
        t.why.cause = rdy_store::transition::Cause::Recheck;
        serde_json::to_vec(&t).unwrap()
    };
    let reference_moves: Vec<Vec<u8>> = opened_moves(&reference)
        .iter()
        .map(|(m, _)| as_recheck(m))
        .collect();
    assert_eq!(
        moves.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
        reference_moves,
        "{}",
        String::from_utf8_lossy(&moves[0].0)
    );
    assert_eq!(transitions_logged(&dest), 1);
    // the causative batch: the one TRANSITION is the last data frame of the batch that writes
    // the history ref complete, and that batch writes nothing else
    let batches = completion_batches(&dest, &s_remote);
    assert_eq!(batches.len(), 1, "{batches:?}");
    let kinds_in: Vec<u8> = batches[0].iter().map(|(k, _, _)| *k).collect();
    assert_eq!(
        kinds_in,
        [kinds::REF_UPDATE, kinds::TRANSITION],
        "{batches:?}"
    );
    assert_eq!(opening_records(&dest), [batches[0][1].1]);
    let engine = Local::open(&dest).unwrap();
    rdy_engine::engine::with_writer_result(&engine, |r| r.reindex()).unwrap();
    engine.close().unwrap();
    assert_eq!(drifts_at(&dest), want);
    assert_eq!(opened_moves(&dest), moves);
}

/// An engine over `inner` that records whether a writer call (a write or a serialized read)
/// is running.
struct Watched {
    inner: Local,
    busy: Arc<AtomicBool>,
}

impl Watched {
    fn call<T>(&self, f: impl FnOnce() -> T) -> T {
        self.busy.store(true, Ordering::SeqCst);
        let out = f();
        self.busy.store(false, Ordering::SeqCst);
        out
    }
}

impl rdy_engine::engine::Engine for Watched {
    fn read(&self) -> Arc<ReadState> {
        self.inner.read()
    }
    fn write(&self, f: rdy_engine::engine::WriteFn) -> Result<serde_json::Value, RdyError> {
        self.call(|| self.inner.write(f))
    }
    fn read_serialized(
        &self,
        f: rdy_engine::engine::ReadFn,
    ) -> Result<serde_json::Value, RdyError> {
        self.call(|| self.inner.read_serialized(f))
    }
    fn open_report(&self) -> OpenReport {
        self.inner.open_report()
    }
    fn root(&self) -> std::path::PathBuf {
        self.inner.root()
    }
    fn briefs(&self) -> Arc<BriefRegistry> {
        self.inner.briefs()
    }
    fn leases(&self) -> Arc<rdy_engine::semantic::lease::LeaseRegistry> {
        self.inner.leases()
    }
}

/// D-GGTYA: a completion's staged-file checks and its full check (the frame walk, the tag rules
/// and the state checks) run on a captured read, never inside a writer call; only the take and
/// the completion ref batch hold the writer.
#[test]
fn r59019_full_check_runs_off_the_writer() {
    use rdy_engine::history_fetch::{HistoryFetchStep, with_check_hook_for_tests};
    let e = env(3);
    let hub = clone_hub(&e, true, 100);
    let dir = private_dir();
    let dest = dir.path().join("clone");
    let s = clone_ready(&e, &hub, &dest).unwrap();
    let busy = Arc::new(AtomicBool::new(false));
    let engine = Watched {
        inner: Local::open(&dest).unwrap(),
        busy: Arc::clone(&busy),
    };
    let api = NoApi::default();
    let clock = Clock::at(T0);
    let renderer = Renderer::default();
    let service = SyncService::new(&dest, &e.user_file, &api, &clock, &hub, &renderer);
    let seen: Arc<Mutex<Vec<(&'static str, bool)>>> = Arc::default();
    let hook = {
        let seen = Arc::clone(&seen);
        let busy = Arc::clone(&busy);
        Box::new(move |p: &'static str| seen.lock().unwrap().push((p, busy.load(Ordering::SeqCst))))
    };
    with_check_hook_for_tests(hook, || {
        let mut fetch = service
            .begin_history(&engine, &writer(), &s.remote)
            .unwrap();
        while let HistoryFetchStep::More(_) = service
            .resume_history_step(&engine, &writer(), &mut fetch)
            .unwrap()
        {}
    });
    let seen = seen.lock().unwrap().clone();
    for p in ["batches", "frames", "tags", "state"] {
        assert!(seen.iter().any(|(q, _)| *q == p), "{p}: {seen:?}");
    }
    assert!(seen.iter().all(|(_, busy)| !busy), "{seen:?}");
    engine.inner.close().unwrap();
}
