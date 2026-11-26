//! The upload pipeline of a history upload: the caller puts the current batch while one
//! preparation worker writes, checks and frames the next one. Two batch slots, five scratch
//! files and one worker at most; every send guard runs at each put. The tests use a recording
//! object wire, the real HTTPS wire against a loopback fixture, and the upload probe's counters
//! and stage hook. Channel waits order the steps; no sleep paces them.
use crate::common;
use crate::hub_tls::{Reply, Server};

use rdy_engine::hub_http::{BodyEncoding, HttpHubTransport};
use rdy_engine::hub_publish_stream::{
    BatchLimits, HistorySent, ObjectWire, PreparedHistory, PreparedObjectBody, PushId,
    UPLOAD_FAILED_FIX, UPLOAD_FAILED_MESSAGE, UploadProbe, UploadStage, send_objects,
    serial_bodies_for_tests, with_upload_probe_for_tests, with_upload_worker_setup_for_tests,
};
use rdy_engine::hub_publish_wire::{HttpPublishWire, ObjectPut, PublishWire, UploadContext};
use rdy_engine::hub_types::{Origin, OriginFence, RequestClaim, Secret};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::codec;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{StreamReader, StreamRecord, check_batch};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ACCESS: &str = "rdyh_a_0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcd2F6gYf";
/// Four records a batch: a canon history of six landings goes in several batches.
const NARROW: BatchLimits = BatchLimits {
    bytes: 2_147_483_648,
    records: 4,
};
/// How long a channel wait may take before the test fails instead of hanging. No assertion
/// reads elapsed time.
const STUCK: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------------------------
// source projects

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "r5907".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// A project of `landings` landings, each a doc with one long prose block that compresses
/// well. Returns the project and each prose block's id.
fn project(dir: &Path, landings: usize) -> (Rdy, Vec<String>) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let mut prose = Vec::new();
    for i in 0..landings {
        let w = write(
            &mut r,
            json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("guide-{i}"), "title": format!("Guide {i}"), "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("Part {i}. {}", "The plan repeats its steps in order. ".repeat(200))}
            ]),
        );
        land(&mut r);
        prose.push(w.created["$p"].clone());
    }
    (r, prose)
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = writer();
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: vec![target.to_string()],
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

/// The canon history of `r` from its head, with no base.
fn canon(r: &Rdy) -> PreparedHistory {
    let read = r.read();
    let head = read.canon_head().unwrap();
    PreparedHistory::canon(read, head, None).unwrap()
}

fn claim() -> RequestClaim {
    RequestClaim::from_session(&Session {
        actor: "agent:claude".into(),
        run: Some("r7".into()),
        role: Role::Writer,
        kind: SessionKind::Agent,
    })
    .unwrap()
}

fn cx(zstd: bool) -> UploadContext {
    UploadContext {
        install: Origin::parse("https://hub.test").unwrap(),
        sync_base: Some("https://hub.test/sync/v1/acme/designs".into()),
        ca_file: None,
        claim: claim(),
        bearer: Secret::new(ACCESS),
        zstd,
    }
}

fn push_id() -> PushId {
    PushId::from_bytes([5; 16])
}

#[track_caller]
fn envelope(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

fn injected(what: &str) -> RdyError {
    RdyError::new(E_IO, format!("injected {what}"), "the test's fix")
}

fn uncertain() -> RdyError {
    RdyError::new(
        E_UPSTREAM_UNCERTAIN,
        "the hub may have taken the batch",
        "the test's fix",
    )
}

// ---------------------------------------------------------------------------------------------
// the recording wire

/// One put as the wire saw it: its push, the encoding and bytes it went as, and the batch's own
/// bytes.
#[derive(Clone, Debug)]
struct Put {
    push: String,
    encoding: BodyEncoding,
    wire: Vec<u8>,
    raw: Vec<u8>,
}

/// Runs at the start of the n-th put (from 0), on the caller; its error fails that put.
type PutHook<'a> = Box<dyn FnMut(usize) -> Result<(), RdyError> + 'a>;

/// An object wire that holds nothing: known names every key, and each put is recorded.
#[derive(Default)]
struct Wire<'a> {
    puts: Mutex<Vec<Put>>,
    hook: Mutex<Option<PutHook<'a>>>,
}

impl<'a> Wire<'a> {
    fn with_hook(f: impl FnMut(usize) -> Result<(), RdyError> + 'a) -> Wire<'a> {
        Wire {
            puts: Mutex::default(),
            hook: Mutex::new(Some(Box::new(f))),
        }
    }

    fn puts(&self) -> Vec<Put> {
        self.puts.lock().unwrap().clone()
    }
}

impl ObjectWire for Wire<'_> {
    fn known(
        &self,
        _: &UploadContext,
        _: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        Ok(keys.to_vec())
    }

    fn put(
        &self,
        _: &UploadContext,
        _: &str,
        _: &mut dyn Read,
        _: u64,
    ) -> Result<ObjectPut, RdyError> {
        panic!("a history batch goes as a prepared body")
    }

    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        let n = self.puts.lock().unwrap().len();
        if let Some(f) = self.hook.lock().unwrap().as_mut() {
            f(n)?;
        }
        let mut raw = Vec::new();
        let (r, len) = body.identity()?;
        r.read_to_end(&mut raw).unwrap();
        assert_eq!((raw.len() as u64, body.raw_len()), (len, len));
        let mut wire = Vec::new();
        let (f, len, encoding) = body.wire(cx.zstd)?;
        f.read_to_end(&mut wire).unwrap();
        assert_eq!(wire.len() as u64, len);
        let mut puts = self.puts.lock().unwrap();
        puts.push(Put {
            push: push.to_string(),
            encoding,
            wire,
            raw,
        });
        Ok(ObjectPut {
            records: puts
                .iter()
                .map(|p| check_batch(&p.raw[..]).unwrap().records)
                .sum(),
            bytes: puts.iter().map(|p| p.raw.len() as u64).sum(),
        })
    }
}

/// The HTTPS publish wire's prepared put as an object wire (known names every key and sends
/// nothing).
struct Https<'a>(&'a HttpPublishWire);

impl ObjectWire for Https<'_> {
    fn known(
        &self,
        _: &UploadContext,
        _: &str,
        keys: &[ObjectKey],
    ) -> Result<Vec<ObjectKey>, RdyError> {
        Ok(keys.to_vec())
    }

    fn put(
        &self,
        _: &UploadContext,
        _: &str,
        _: &mut dyn Read,
        _: u64,
    ) -> Result<ObjectPut, RdyError> {
        panic!("a history batch goes as a prepared body")
    }

    fn put_prepared(
        &self,
        cx: &UploadContext,
        push: &str,
        body: &mut PreparedObjectBody,
    ) -> Result<ObjectPut, RdyError> {
        self.0.put_objects_prepared(cx, push, body)
    }
}

/// A loopback HTTPS hub, its wire and a context for it: `(server, wire, ca file)`.
fn https(dir: &Path) -> (Server, HttpPublishWire, PathBuf) {
    let s = Server::start();
    let ca = dir.join("ca.pem");
    std::fs::write(&ca, s.ca_pem()).unwrap();
    let origin = Origin::parse(s.origin()).unwrap();
    let fence = OriginFence::new(origin, vec![]).unwrap();
    let wire = HttpPublishWire::new(HttpHubTransport::new(fence).unwrap());
    (s, wire, ca)
}

fn https_cx(s: &Server, ca: &Path, zstd: bool, bearer: &str) -> UploadContext {
    UploadContext {
        install: Origin::parse(s.origin()).unwrap(),
        sync_base: Some(format!("{}/sync/v1/acme/designs", s.origin())),
        ca_file: Some(ca.to_path_buf()),
        claim: claim(),
        bearer: Secret::new(bearer),
        zstd,
    }
}

/// Queues `n` objects/put replies with growing counters.
fn put_replies(s: &mut Server, n: usize) {
    for i in 1..=n {
        s.reply(Reply::json(
            200,
            &format!(r#"{{"records":{i},"bytes":{}}}"#, i * 10),
        ));
    }
    s.accept_n(n);
}

/// What one upload returned: its result, what the hub acknowledged and the contexts it took.
struct Run {
    result: Result<(), RdyError>,
    sent: HistorySent,
    contexts: usize,
}

/// Sends every record of `history` that `missing` names under [`push_id`] through `wire`; the
/// n-th context (from 0) is `context(n)`.
fn run_missing(
    history: &PreparedHistory,
    missing: &[ObjectKey],
    wire: &dyn ObjectWire,
    mut context: impl FnMut(usize) -> Result<UploadContext, RdyError>,
) -> Run {
    let mut sent = HistorySent::default();
    let mut contexts = 0;
    let result = {
        let mut next = || {
            let cx = context(contexts);
            contexts += 1;
            cx
        };
        send_objects(
            history,
            missing,
            &push_id(),
            NARROW,
            wire,
            &mut next,
            &mut || Ok(()),
            &mut sent,
        )
    };
    Run {
        result,
        sent,
        contexts,
    }
}

fn run(
    history: &PreparedHistory,
    wire: &dyn ObjectWire,
    context: impl FnMut(usize) -> Result<UploadContext, RdyError>,
) -> Run {
    run_missing(history, &history.keys(), wire, context)
}

/// [`run`] with `probe` counting the upload.
fn probed(
    probe: &Arc<UploadProbe>,
    history: &PreparedHistory,
    wire: &dyn ObjectWire,
    context: impl FnMut(usize) -> Result<UploadContext, RdyError>,
) -> Run {
    with_upload_probe_for_tests(probe.clone(), || run(history, wire, context))
}

/// A stage hook that sends each stage, place and preparing thread's name to the returned
/// receiver.
fn stage_events(probe: &UploadProbe) -> mpsc::Receiver<(UploadStage, usize, String)> {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    *probe.on_stage.lock().unwrap() = Some(Box::new(move |stage, place| {
        let name = std::thread::current().name().unwrap_or("").to_string();
        let _ = tx.lock().unwrap().send((stage, place, name));
        Ok(())
    }));
    rx
}

/// Waits for `stage` of batch `place` on `rx`; returns the events seen up to it.
fn wait_for(
    rx: &mpsc::Receiver<(UploadStage, usize, String)>,
    stage: UploadStage,
    place: usize,
) -> Vec<(UploadStage, usize, String)> {
    let mut seen = Vec::new();
    loop {
        let e = rx
            .recv_timeout(STUCK)
            .unwrap_or_else(|_| panic!("{stage:?} of batch {place} never came: {seen:?}"));
        let done = e.0 == stage && e.1 == place;
        seen.push(e);
        if done {
            return seen;
        }
    }
}

/// No body, scratch file or worker of the upload is left.
#[track_caller]
fn all_closed(probe: &UploadProbe) {
    assert_eq!(probe.live_bodies.load(Ordering::SeqCst), 0, "bodies");
    assert_eq!(probe.live_scratch.load(Ordering::SeqCst), 0, "scratch");
    assert_eq!(probe.live_workers.load(Ordering::SeqCst), 0, "workers");
}

fn keys_of(raw: &[u8]) -> Vec<ObjectKey> {
    check_batch(raw).unwrap().keys
}

/// Overwrites one byte in the middle of `kind` record `hash`'s stored payload.
fn corrupt(r: &Rdy, kind: u8, hash: &Hash) {
    let reader = r.store().reader();
    let (_, payload) = reader.get_kind_raw(kind, hash).unwrap();
    let segments = reader.dir().join("segments");
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
    let mid = (*at + payload.len() / 2) as u64;
    f.seek(SeekFrom::Start(mid)).unwrap();
    f.write_all(&[payload[payload.len() / 2] ^ 0xa5]).unwrap();
    f.sync_all().unwrap();
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn r5907_batches_are_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(&dir.path().join("p"), 6);
    let h = canon(&r);
    for zstd in [false, true] {
        let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, zstd).unwrap();
        assert!(reference.len() >= 4, "{} batches", reference.len());
        if zstd {
            assert!(
                reference.iter().all(|(_, f)| f.is_some()),
                "each frame is smaller"
            );
        }
        let wire = Wire::default();
        let probe = Arc::new(UploadProbe::default());
        let out = probed(&probe, &h, &wire, |_| Ok(cx(zstd)));
        out.result.unwrap();
        let puts = wire.puts();
        assert_eq!(puts.len(), reference.len());
        assert_eq!(out.contexts, puts.len(), "one fresh context a put");
        for (p, (raw, framed)) in puts.iter().zip(&reference) {
            assert_eq!(p.push, push_id().as_str());
            assert_eq!(&p.raw, raw, "the same stream bytes");
            match framed {
                Some(f) => assert_eq!((p.encoding, &p.wire), (BodyEncoding::Zstd, f)),
                None => assert_eq!((p.encoding, &p.wire), (BodyEncoding::Identity, raw)),
            }
        }
        // the same keys in the same batch boundaries
        let got: Vec<_> = puts.iter().map(|p| keys_of(&p.raw)).collect();
        let want: Vec<_> = reference.iter().map(|(raw, _)| keys_of(raw)).collect();
        assert_eq!(got, want);
        assert_eq!(
            out.sent,
            HistorySent {
                batches: puts.len() as u32,
                records: want.iter().map(|k| k.len() as u64).sum(),
                bytes: reference.iter().map(|(raw, _)| raw.len() as u64).sum(),
            }
        );
        // each batch is framed once, on the preparing thread, and never again at the put
        let framed = if zstd { puts.len() } else { 0 };
        assert_eq!(probe.compressed.load(Ordering::SeqCst), framed);
        all_closed(&probe);
    }

    // the HTTPS wire sends the prepared bytes as they are: the frame with its encoding, or the
    // raw batch as identity
    for zstd in [true, false] {
        let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, zstd).unwrap();
        let (mut s, http, ca) = https(dir.path());
        put_replies(&mut s, reference.len());
        let probe = Arc::new(UploadProbe::default());
        let out = probed(&probe, &h, &Https(&http), |_| {
            Ok(https_cx(&s, &ca, zstd, ACCESS))
        });
        out.result.unwrap();
        let requests = s.requests();
        assert_eq!(requests.len(), reference.len());
        for (req, (raw, framed)) in requests.iter().zip(&reference) {
            assert_eq!(req.method, "POST");
            assert!(req.target.contains("/objects/put?push="), "{}", req.target);
            match framed {
                Some(f) => {
                    assert_eq!(req.header("content-encoding"), Some("zstd"));
                    assert_eq!(&req.body, f);
                }
                None => {
                    assert_eq!(req.header("content-encoding"), None);
                    assert_eq!(&req.body, raw);
                }
            }
        }
        let framed = if zstd { reference.len() } else { 0 };
        assert_eq!(probe.compressed.load(Ordering::SeqCst), framed);
        all_closed(&probe);
        assert_eq!(s.finish(), 0);
    }
}

#[test]
fn r5907_next_batch_builds_and_compresses_during_put() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(dir.path(), 6);
    let h = canon(&r);
    let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, true).unwrap();
    let batches = reference.len();
    let probe = Arc::new(UploadProbe::default());
    let rx = stage_events(&probe);
    let order = RefCell::new(Vec::new());
    // each put but the last is held until the next batch is written, checked and framed
    let wire = Wire::with_hook(|n| {
        if n + 1 < batches {
            let seen = wait_for(&rx, UploadStage::Compressed, n + 1);
            order.borrow_mut().extend(seen);
        }
        Ok(())
    });
    let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
    out.result.unwrap();
    let order = order.take();
    for place in 1..batches {
        let at = |stage| {
            order
                .iter()
                .position(|e| e.0 == stage && e.1 == place)
                .unwrap_or_else(|| panic!("{stage:?} {place}: {order:?}"))
        };
        assert!(at(UploadStage::Built) < at(UploadStage::Compressed));
        let worker = &order[at(UploadStage::Built)].2;
        assert_eq!(worker, "rdy-upload-prepare", "batch {place}");
    }
    // the first batch is prepared on the caller, before the first put
    assert_eq!(order[0].1, 0);
    assert_ne!(order[0].2, "rdy-upload-prepare");
    assert_eq!(probe.compressed.load(Ordering::SeqCst), batches);
    let puts = wire.puts();
    assert_eq!(puts.len(), batches);
    for (p, (raw, framed)) in puts.iter().zip(&reference) {
        assert_eq!(&p.raw, raw);
        assert_eq!(Some(&p.wire), framed.as_ref());
    }
    all_closed(&probe);
}

#[test]
fn r5907_two_batches_keep_injected_encoding() {
    let version = codec::latest(kinds::LANDING) + 1;
    let _caller = codec::test_encoding(kinds::LANDING, version);
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(dir.path(), 6);
    let h = canon(&r);
    let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, false).unwrap();
    assert!(reference.len() >= 2);
    let injected_landings = |raw: &[u8]| {
        let (mut reader, _) = StreamReader::open(raw).unwrap();
        let mut n = 0;
        while let Some(rec) = reader.next().unwrap() {
            if let StreamRecord::Present { key, encoding, .. } = rec
                && key.kind == kinds::LANDING
            {
                assert_eq!(
                    encoding, version,
                    "every landing is in the injected encoding"
                );
                n += 1;
            }
        }
        n
    };
    let later: usize = reference[1..]
        .iter()
        .map(|(raw, _)| injected_landings(raw))
        .sum();
    assert!(
        later > 0,
        "a batch the worker prepares holds an injected landing"
    );

    // without the hook the worker checks with production's encodings and refuses
    let wire = Wire::default();
    let probe = Arc::new(UploadProbe::default());
    let out = probed(&probe, &h, &wire, |_| Ok(cx(false)));
    let e = out.result.unwrap_err();
    assert!(wire.puts().len() < reference.len(), "{e}");
    all_closed(&probe);

    // the hook makes the same registration on the worker; its guard drops with the worker
    let made = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    struct Guard(#[allow(dead_code)] codec::TestEncoding, Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }
    let setup: rdy_engine::hub_publish_stream::UploadWorkerSetup = {
        let (made, dropped) = (made.clone(), dropped.clone());
        Arc::new(move || {
            made.fetch_add(1, Ordering::SeqCst);
            Box::new(Guard(
                codec::test_encoding(kinds::LANDING, version),
                dropped.clone(),
            ))
        })
    };
    let wire = Wire::default();
    let probe = Arc::new(UploadProbe::default());
    let out =
        with_upload_worker_setup_for_tests(setup, || probed(&probe, &h, &wire, |_| Ok(cx(false))));
    out.result.unwrap();
    assert_eq!(probe.workers_started.load(Ordering::SeqCst), 1);
    assert_eq!(made.load(Ordering::SeqCst), 1);
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "the worker's guard dropped at join"
    );
    assert_eq!(
        codec::latest(kinds::LANDING),
        version,
        "the caller's registration stays"
    );
    let puts = wire.puts();
    assert_eq!(puts.len(), reference.len());
    for (p, (raw, _)) in puts.iter().zip(&reference) {
        assert_eq!(&p.raw, raw);
        // each batch decodes and hashes with the injected encoding and passes the batch check
        check_batch(&p.raw[..]).unwrap();
        rdy_engine::stream_check::check_batch(&p.raw[..], None).unwrap();
    }
    let sent: usize = puts[1..].iter().map(|p| injected_landings(&p.raw)).sum();
    assert_eq!(sent, later);
    all_closed(&probe);
}

#[test]
fn r5907_two_slots_and_one_worker_are_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(dir.path(), 6);
    let h = canon(&r);
    let batches = serial_bodies_for_tests(&h, &h.keys(), NARROW, true)
        .unwrap()
        .len();
    assert!(batches >= 4);
    let probe = Arc::new(UploadProbe::default());
    let rx = stage_events(&probe);
    let held = RefCell::new(None);
    let wire = Wire::with_hook(|n| {
        if n == 0 {
            // the put of batch 0 is held: batch 1 gets prepared, batch 2 never starts
            wait_for(&rx, UploadStage::Compressed, 1);
            // no wait: batch 2's job is sent only after body 0 is taken, so a third stage event
            // cannot be queued while the put of batch 0 is held (`max_bodies` below pins the bound)
            let next = rx.try_recv();
            *held.borrow_mut() = Some((
                next.ok(),
                probe.live_bodies.load(Ordering::SeqCst),
                probe.live_workers.load(Ordering::SeqCst),
            ));
        }
        Ok(())
    });
    let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
    out.result.unwrap();
    let (next, bodies, workers) = held.take().unwrap();
    assert_eq!(next, None, "no third batch while two are held");
    assert_eq!((bodies, workers), (2, 1));
    assert_eq!(probe.max_bodies.load(Ordering::SeqCst), 2);
    assert!(probe.max_scratch.load(Ordering::SeqCst) <= 5);
    assert_eq!(probe.max_workers.load(Ordering::SeqCst), 1);
    assert_eq!(probe.workers_started.load(Ordering::SeqCst), 1);
    assert_eq!(wire.puts().len(), batches);
    all_closed(&probe);

    // one batch is prepared on the caller alone, and no batch starts no worker
    let probe = Arc::new(UploadProbe::default());
    let wire = Wire::default();
    let one = h.keys()[..1].to_vec();
    let out = with_upload_probe_for_tests(probe.clone(), || {
        run_missing(&h, &one, &wire, |_| Ok(cx(true)))
    });
    out.result.unwrap();
    assert_eq!(wire.puts().len(), 1);
    assert_eq!(probe.workers_started.load(Ordering::SeqCst), 0);
    let out = with_upload_probe_for_tests(probe.clone(), || {
        run_missing(&h, &[], &Wire::default(), |_| Ok(cx(true)))
    });
    out.result.unwrap();
    assert_eq!(out.contexts, 0);
    assert_eq!(probe.workers_started.load(Ordering::SeqCst), 0);
    all_closed(&probe);
}

#[test]
fn r5907_pipeline_failures_join_and_keep_resume_order() {
    let dir = tempfile::tempdir().unwrap();
    let (r, prose) = project(dir.path(), 6);
    let h = canon(&r);
    let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, true).unwrap();
    let batches = reference.len();
    assert!(batches >= 4);
    let fail_stage = |probe: &UploadProbe, at: UploadStage, place: usize, e: RdyError| {
        let e = Mutex::new(Some(e));
        *probe.on_stage.lock().unwrap() = Some(Box::new(move |stage, p| {
            match e.lock().unwrap().take_if(|_| stage == at && p == place) {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }));
    };
    // (case, its error, the puts acknowledged before it, the run, its probe)
    let mut cases: Vec<(&str, RdyError, usize, Run, Arc<UploadProbe>)> = Vec::new();
    for (name, at, place) in [
        ("first build", UploadStage::Built, 0),
        ("next build", UploadStage::Built, 2),
        ("first compression", UploadStage::Compressed, 0),
        ("next compression", UploadStage::Compressed, 1),
    ] {
        let probe = Arc::new(UploadProbe::default());
        fail_stage(&probe, at, place, injected(name));
        let wire = Wire::default();
        let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
        assert_eq!(wire.puts().len(), place, "{name}");
        cases.push((name, injected(name), place, out, probe));
    }
    {
        let probe = Arc::new(UploadProbe::default());
        let wire = Wire::default();
        let out = probed(&probe, &h, &wire, |n| match n {
            1 => Err(injected("context")),
            _ => Ok(cx(true)),
        });
        assert_eq!(wire.puts().len(), 1);
        cases.push(("context", injected("context"), 1, out, probe));
    }
    {
        let probe = Arc::new(UploadProbe::default());
        let wire = Wire::with_hook(|n| match n {
            1 => Err(uncertain()),
            _ => Ok(()),
        });
        let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
        assert_eq!(wire.puts().len(), 1);
        cases.push(("put", uncertain(), 1, out, probe));
    }
    {
        // a worker panic: core's own words, never the payload
        let probe = Arc::new(UploadProbe::default());
        *probe.on_stage.lock().unwrap() = Some(Box::new(|stage, place| {
            if stage == UploadStage::Built && place == 1 {
                panic!("SENTINEL-PANIC");
            }
            Ok(())
        }));
        let wire = Wire::default();
        let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
        assert_eq!(wire.puts().len(), 1);
        let want = RdyError::new(E_INTERNAL, UPLOAD_FAILED_MESSAGE, UPLOAD_FAILED_FIX);
        assert!(!format!("{:?}", out.result).contains("SENTINEL"));
        cases.push(("worker panic", want, 1, out, probe));
    }
    {
        // an earlier uncertain put wins over a later preparation failure
        let probe = Arc::new(UploadProbe::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        *probe.on_stage.lock().unwrap() = Some(Box::new(move |stage, place| {
            if stage == UploadStage::Built && place == 1 {
                tx.lock().unwrap().send(()).unwrap();
                return Err(injected("later build"));
            }
            Ok(())
        }));
        let wire = Wire::with_hook(|n| {
            if n == 0 {
                rx.recv_timeout(STUCK)
                    .expect("the later build failed first");
                return Err(uncertain());
            }
            Ok(())
        });
        let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
        assert_eq!(wire.puts().len(), 0);
        cases.push(("uncertain first", uncertain(), 0, out, probe));
    }
    for (name, want, acked, out, probe) in &cases {
        let got = out.result.as_ref().unwrap_err();
        assert_eq!(envelope(got), envelope(want), "{name}");
        assert_eq!(out.sent.batches as usize, *acked, "{name}");
        assert!(probe.workers_started.load(Ordering::SeqCst) <= 1, "{name}");
        all_closed(probe);
    }

    // the next command resumes the same push: what the hub lacks goes in the original order
    let wire = Wire::with_hook(|n| match n {
        2 => Err(uncertain()),
        _ => Ok(()),
    });
    let first = run(&h, &wire, |_| Ok(cx(true)));
    assert_eq!(
        envelope(first.result.as_ref().unwrap_err()),
        envelope(&uncertain())
    );
    let acked: Vec<ObjectKey> = wire.puts().iter().flat_map(|p| keys_of(&p.raw)).collect();
    let mut lacking: Vec<ObjectKey> = h
        .keys()
        .into_iter()
        .filter(|k| !acked.contains(k))
        .collect();
    lacking.sort();
    let resumed = Wire::default();
    run_missing(&h, &lacking, &resumed, |_| Ok(cx(true)))
        .result
        .unwrap();
    let mut order = acked.clone();
    order.extend(resumed.puts().iter().flat_map(|p| keys_of(&p.raw)));
    let want: Vec<ObjectKey> = reference.iter().flat_map(|(raw, _)| keys_of(raw)).collect();
    assert_eq!(order, want);
    assert!(
        wire.puts()
            .iter()
            .chain(&resumed.puts())
            .all(|p| p.push == push_id().as_str())
    );

    // an epoch change during a put: the next put is refused and nothing later goes
    let r = RefCell::new(r);
    let probe = Arc::new(UploadProbe::default());
    let wire = Wire::with_hook(|n| {
        if n == 0 {
            remove(&mut r.borrow_mut(), &prose[0]);
        }
        Ok(())
    });
    let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
    assert_eq!(out.result.unwrap_err().code, E_REDACTED_STATE);
    assert_eq!((wire.puts().len(), out.sent.batches), (1, 1));
    all_closed(&probe);
}

#[test]
fn r5907_pipeline_rechecks_all_send_guards() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = project(&dir.path().join("p"), 6);
    let h = canon(&r);
    let reference = serial_bodies_for_tests(&h, &h.keys(), NARROW, true).unwrap();
    let batches = reference.len();
    assert!(batches >= 4);

    // a fresh context that no longer takes zstd sends the raw batch as identity
    let wire = Wire::default();
    let out = run(&h, &wire, |n| Ok(cx(n == 0)));
    out.result.unwrap();
    let puts = wire.puts();
    assert_eq!(puts.len(), batches);
    assert_eq!(Some(&puts[0].wire), reference[0].1.as_ref());
    for (p, (raw, _)) in puts[1..].iter().zip(&reference[1..]) {
        assert_eq!(
            (p.encoding, &p.wire, &p.raw),
            (BodyEncoding::Identity, raw, raw)
        );
    }

    // a fresh context whose token cannot go in a header: refused before the request
    let (mut s, http, ca) = https(dir.path());
    put_replies(&mut s, 1);
    let probe = Arc::new(UploadProbe::default());
    let out = probed(&probe, &h, &Https(&http), |n| {
        let token = if n == 0 {
            ACCESS
        } else {
            "rdyh_a_SENTINEL\nHEADER"
        };
        Ok(https_cx(&s, &ca, true, token))
    });
    let e = out.result.unwrap_err();
    assert_eq!(e.code, E_UPSTREAM_TOKEN, "{e}");
    assert!(!format!("{e:?}").contains("SENTINEL"));
    assert_eq!(out.sent.batches, 1);
    assert_eq!(s.requests().len(), 1);
    all_closed(&probe);
    assert_eq!(s.finish(), 0);

    // a record of a later batch damaged after the walk: the worker's read refuses it
    let late = keys_of(&reference[2].0);
    let change = late
        .iter()
        .find(|k| k.kind == kinds::CHANGE)
        .copied()
        .unwrap_or(late[0]);
    let probe = Arc::new(UploadProbe::default());
    let wire = Wire::with_hook(|n| {
        if n == 0 {
            corrupt(&r, change.kind, &change.hash);
        }
        Ok(())
    });
    let out = probed(&probe, &h, &wire, |_| Ok(cx(true)));
    let e = out.result.unwrap_err();
    assert_eq!(e.code, E_IO, "{e}");
    assert_eq!(
        wire.puts().len(),
        2,
        "batches 0 and 1 go, batch 2 is refused"
    );
    all_closed(&probe);

    // a removal during a put: the epoch check refuses the next put
    let dir2 = tempfile::tempdir().unwrap();
    let (r2, prose2) = project(dir2.path(), 6);
    let h2 = canon(&r2);
    let r2 = RefCell::new(r2);
    let probe = Arc::new(UploadProbe::default());
    let wire = Wire::with_hook(|n| {
        if n == 1 {
            remove(&mut r2.borrow_mut(), &prose2[5]);
        }
        Ok(())
    });
    let out = probed(&probe, &h2, &wire, |_| Ok(cx(true)));
    let e = out.result.unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e}");
    assert_eq!((wire.puts().len(), out.sent.batches), (2, 2));
    all_closed(&probe);
}
