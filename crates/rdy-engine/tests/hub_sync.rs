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
use rdy_engine::hub_publish_wire::{SYNC_MEDIA_TYPE, UploadContext};
use rdy_engine::hub_sync::{
    CanonPreview, PreviewKind, PullRequest, PushOutcome, PushRequest, WriteEnd, canon_preview,
};
use rdy_engine::hub_sync_wire::{
    GateSettings, GetRequest, HttpSyncWire, HubRequest, KnownPull, LookupOutcome, ProposeGate,
    SyncStateRequest, SyncWire, WriteFailure, WriteKind, WriteRequest as SyncWrite, WriteTag,
};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::landing_check::CheckInputs;
use rdy_engine::readiness::assessment::pack_digest;
use rdy_engine::*;
use rdy_model::Hash;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{
    KIND_CANON, SYNC_LOCKED_FIX, SyncCanon, SyncHeld, SyncPending, SyncStep, SyncUpload,
    sync_locked_message,
};
use rdy_store::sync_stream::StreamPurpose;
use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::atomic::Ordering;
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
    assert_eq!(hub.steps(), ["discovery", "state", "known", "put", "write"]);
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
        render_inputs: inputs,
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
                Err(WriteFailure::Refused(e)) => e,
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
    let hub = Hub::new().with_state(|s| s.batch_records = 2);
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
    // each upload's known, put and write come after the previous write
    assert_eq!(
        hub.steps(),
        [
            "discovery",
            "state",
            "known",
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
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!(
        (c.applied, c.held),
        (None, Some(SyncHeld { head, request: 7 }))
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
            "known" if current.is_none() => current = c.push.clone(),
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
        assert_eq!(w.render_inputs.rdy, rdy);
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
            pack_digest: pack_digest(&writes[0].render_inputs.pack),
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
    assert_eq!(w.render_inputs.rdy, rdy);
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
        Some(SyncHeld { head, request: 3 })
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
        request, head: h, ..
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
            request: 7
        })
    );
}

/// D-H0BZN "Held": `held` clears when `applied` reaches its head, by a taken write or by a
/// pull. The clear on a request that closed with no landing needs the held push's outcome
/// (open question Q1); it waits for the held pins of the hub's fixture v3.
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
    push(&e, &hub, None).unwrap();
    // a pull while hub canon is below the held head keeps held
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
                request: 5
            })
        )
    );
    hub.state.lock().unwrap().canon = Some(head2);
    s.pull(PullRequest::default()).unwrap();
    let c = canon_of(&e, "shared").unwrap();
    assert_eq!((c.applied, c.held), (Some(head2), None));
    let _ = head;
    // v3 skip: "the request has closed with no landing" (Q1) has no outcome to read yet
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
        Some(SyncHeld { head, request: 4 })
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
        ["discovery", "state", "outcome", "known", "put", "write"]
    );
    let (id, ..) = taken(&out);
    assert_ne!(id, P1);
    assert_eq!(pending_of(&e, "shared"), None);

    // F7: the lookup itself refused (the write intent is open): pending stays, nothing else
    let e = env(1);
    let head = e.landings[0];
    let sent = seed_pending(&e, P1, head, T0);
    let hub = Hub::new();
    hub.fail_at("outcome", 1, hub_words("E_HUB_BUSY"));
    let x = err(push(&e, &hub, None));
    assert_eq!(x.code, "E_HUB_BUSY");
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
    assert_eq!(pending_of(&e, "shared"), Some(sent.clone()));
    // E_HUB_NOT_FOUND (Q5): kept too
    let hub = Hub::new();
    hub.fail_at("outcome", 1, hub_words("E_HUB_NOT_FOUND"));
    assert_eq!(err(push(&e, &hub, None)).code, "E_HUB_NOT_FOUND");
    assert_eq!(pending_of(&e, "shared"), Some(sent.clone()));

    // on pull: the lookup comes first too
    let hub = Hub::new();
    hub.fail_at("outcome", 1, hub_words("E_HUB_BUSY"));
    let x = err(Sync::new(&e, &hub).pull(PullRequest::default()));
    assert_eq!(x.code, "E_HUB_BUSY");
    assert_eq!(hub.steps(), ["discovery", "state", "outcome"]);
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
        ["discovery", "state", "outcome", "known", "put", "write"]
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
    hub.fail_at("outcome", 1, hub_words("E_HUB_BUSY"));
    err(push(&e, &hub, None));
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

    // the preview reads this store only: the hub tip is not here, so the list is short
    let read = e.engine.read();
    let p = canon_preview(&read, "shared", Some(theirs), PreviewKind::ForcePush).unwrap();
    assert_eq!(
        p,
        CanonPreview {
            remote: "shared".into(),
            before: Some(theirs),
            after: Some(head),
            leaving: vec![],
            complete: true,
        }
    );
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
