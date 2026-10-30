//! hpb17 (D-91DZW, D-D2NPN, D-BPYN8, D-KDFZ3, D-Z8Q4N, D-075QD, D-WDKWQ): the publication take
//! under the holder's one writer. Core's checks, the profile facts, the host verdict and the
//! append run in that order inside one take: a receiver cap or changed input after the verdict
//! appends nothing, no other write enters the interval, and every Publications limit refuses
//! before the root landing is materialized or any blob payload is read.
//!
//! These cases drive the engine's take directly (and `Local` for the writer case); the adapter's
//! cases compare the same facts with `Snapshot::profiles`.

use rdy_engine::engine::{Engine, Local, with_writer_result};
use rdy_engine::publication::{self, CheckStatus, Limits};
use rdy_engine::publication_take::{
    PublicationFacts, PublicationRefusal, PublicationTarget, blob_payload_reads_for_tests,
    materializations_for_tests, take_publication,
};
use rdy_engine::redaction::RedactInput;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::LandRequest;
use rdy_store::redaction_import::{ReceiverCaps, set_receiver_caps_for_tests};
use rdy_store::segment::Location;
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};

const CANARY: &str = "ZEBRA-7731";

// ---------------------------------------------------------------------------------------------
// source projects

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hpb17".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "hpb17",
            Some("test summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

fn repo(r: &Rdy) -> RepoId {
    r.store().repo_id().unwrap().unwrap()
}

/// The figure's bytes: one full store chunk (1 MiB) and five bytes more, so its blob is two
/// chunks and a facts walk that read chunk payloads would show in the source's count.
fn figure() -> Vec<u8> {
    (0..(1u32 << 20) + 5).map(|i| (i % 251) as u8).collect()
}

/// A source project: doc `guide` with a prose block that holds the canary and a figure, and an
/// org profile `site` that selects the doc, landed and tagged `v1.0.0`. Returns the project, the
/// profile's id, the prose id and the tag.
fn source(dir: &Path) -> (Rdy, String, String, Hash) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(&figure()).unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (doc, prose, admin) = (
        w.created["$d"].clone(),
        w.created["$p"].clone(),
        w.created["$admin"].clone(),
    );
    let w = write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$s", "parent": admin,
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [doc]}}]),
    );
    let profile = w.created["$s"].clone();
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    (r, profile, prose, v1)
}

/// Removes every version of block `target` through the store's own landing path.
fn remove(r: &mut Rdy, target: &str) {
    let s = session(Role::Writer);
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

/// The whole history of `tag` as one batch, as the export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

fn path_of(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    p
}

fn files(dir: &Path, name: &str, bytes: &[u8]) -> Vec<File> {
    vec![File::open(path_of(dir, name, bytes)).unwrap()]
}

fn target(tag: &str, landing: Hash, profile: &str) -> PublicationTarget {
    PublicationTarget {
        upload: UploadTarget {
            tag: tag.into(),
            landing,
        },
        profile: profile.into(),
    }
}

// ---------------------------------------------------------------------------------------------
// holders

fn holder(dir: &Path, repo: RepoId) -> (PathBuf, Rdy) {
    let root = dir.join("holder");
    let h = Rdy::create_holder(&root, repo).unwrap();
    (root, h)
}

fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root.join(".rdy/store/segments")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

/// What a refused take must leave as it was: the log's end and bytes, the refs, the tag list
/// and the removal epoch.
type State = (
    Location,
    BTreeMap<String, Vec<u8>>,
    Vec<(String, Vec<u8>)>,
    Vec<String>,
    u64,
);

fn state(root: &Path, h: &Rdy) -> State {
    let mut refs = h.store().refs_with_prefix("tag/").unwrap();
    refs.extend(h.store().refs_with_prefix("answer/").unwrap());
    let tags = h
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    (
        h.store().log_end(),
        segments(root),
        refs,
        tags,
        h.store().redaction_epoch(),
    )
}

/// The core error of a refusal; a host verdict here is a test failure.
fn core<E: std::fmt::Debug>(r: Result<impl std::fmt::Debug, PublicationRefusal<E>>) -> RdyError {
    match r {
        Err(PublicationRefusal::Core(e)) => e,
        other => panic!("expected a core refusal, got {other:?}"),
    }
}

/// A verdict that counts its calls and passes.
fn counting(n: &Arc<AtomicUsize>) -> impl FnOnce(&PublicationFacts) -> Result<(), ()> + use<> {
    let n = Arc::clone(n);
    move |_| {
        n.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb17_caps_follow_one_verdict_and_append_nothing() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (mut r, profile, prose, _) = source(&dir.path().join("src"));
    remove(&mut r, &prose);
    let v2 = tag(&mut r, "v2.0.0");
    let l2 = landing_of(&r, v2);
    let bytes = export(&r, v2);
    let records = rdy_engine::stream_check::check_batch(bytes.as_slice(), Some(repo(&r)))
        .unwrap()
        .keys
        .len() as u64;
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = state(&root, &h);
    let calls = Arc::new(AtomicUsize::new(0));

    // a frames cap the holder's log is within, and the batch is not: the scan before the
    // verdict passes, and the batch's frames are refused after it
    set_receiver_caps_for_tests(Some(ReceiverCaps {
        frames: records,
        ..ReceiverCaps::DEFAULT
    }));
    let got = take_publication(
        &mut h,
        &files(dir.path(), "up", &bytes),
        target("v2.0.0", l2, &profile),
        counting(&calls),
    );
    set_receiver_caps_for_tests(None);
    let e = core(got);
    assert_eq!(e.code, E_LIMIT, "{e:?}");
    assert!(e.message.ends_with("receiver frames"), "{}", e.message);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one verdict before the cap"
    );
    assert_eq!(state(&root, &h), before, "nothing appended");

    // the same handle takes it under the default caps
    let got = take_publication(
        &mut h,
        &files(dir.path(), "up", &bytes),
        target("v2.0.0", l2, &profile),
        counting(&calls),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!got.outcome.unchanged);
    assert_eq!(got.facts.tag_hash, v2);
    h.fsck().unwrap();
}

#[test]
fn hpb17_changed_input_after_verdict_rolls_back_the_append() {
    let dir = tempfile::tempdir().unwrap();
    let (r, profile, _, v1) = source(&dir.path().join("src"));
    let l1 = landing_of(&r, v1);
    let bytes = export(&r, v1);
    let at = bytes
        .windows(CANARY.len())
        .position(|w| w == CANARY.as_bytes())
        .unwrap() as u64;
    let path = path_of(dir.path(), "up", &bytes);
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = state(&root, &h);

    // the verdict changes one record's bytes through the test's own handle: the append reads
    // each record again and refuses, and its unacknowledged bytes are rolled back
    let changed = path.clone();
    let got = take_publication(
        &mut h,
        &[File::open(&path).unwrap()],
        target("v1.0.0", l1, &profile),
        move |facts: &PublicationFacts| {
            assert!(facts.profile.is_some());
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&changed)
                .unwrap();
            f.seek(SeekFrom::Start(at)).unwrap();
            f.write_all(b"X").unwrap();
            f.sync_all().unwrap();
            Ok::<(), ()>(())
        },
    );
    let e = core(got);
    assert!(
        e.message.ends_with("input changed during take"),
        "{}",
        e.message
    );
    assert_eq!(state(&root, &h), before, "the append is rolled back");

    // the file as it was: the same handle takes it
    std::fs::write(&path, &bytes).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let got = take_publication(
        &mut h,
        &[File::open(&path).unwrap()],
        target("v1.0.0", l1, &profile),
        counting(&calls),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(got.outcome.tag_hash, v1);
    h.fsck().unwrap();
}

#[test]
fn hpb17_writer_covers_facts_verdict_and_append() {
    let dir = tempfile::tempdir().unwrap();
    let (r, profile, _, v1) = source(&dir.path().join("src"));
    let l1 = landing_of(&r, v1);
    let fs = files(dir.path(), "up", &export(&r, v1));
    let (_root, mut h) = holder(dir.path(), repo(&r));
    let events: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = Arc::clone(&events);
    h.store_mut()
        .on_take_append_for_tests(move |i| seen.lock().unwrap().push(format!("append {i}")));
    let local = Arc::new(Local::new(h));

    // the competing write starts only once the verdict runs, under the take's writer
    let gate = Arc::new(Barrier::new(2));
    let (done_tx, done_rx) = mpsc::channel();
    let competitor = {
        let (local, gate, events) = (Arc::clone(&local), Arc::clone(&gate), Arc::clone(&events));
        std::thread::spawn(move || {
            gate.wait();
            local
                .write(Box::new(move |_| {
                    events.lock().unwrap().push("competitor".into());
                    Ok(Value::Null)
                }))
                .unwrap();
            done_tx.send(()).unwrap();
        })
    };
    let (verdict_events, verdict_gate) = (Arc::clone(&events), Arc::clone(&gate));
    let target = target("v1.0.0", l1, &profile);
    let taken = with_writer_result(&*local, move |rdy| {
        Ok(take_publication(rdy, &fs, target, move |facts| {
            verdict_events
                .lock()
                .unwrap()
                .push(format!("verdict {}", facts.tag));
            verdict_gate.wait();
            Ok::<(), ()>(())
        }))
    })
    .unwrap()
    .unwrap_or_else(|e| panic!("{e:?}"));
    done_rx.recv().unwrap();
    competitor.join().unwrap();

    let events = events.lock().unwrap().clone();
    let appends = taken.outcome.records_taken as usize;
    assert!(appends > 0);
    assert_eq!(events.len(), appends + 2, "{events:?}");
    assert_eq!(events[0], "verdict v1.0.0");
    for (i, e) in events[1..=appends].iter().enumerate() {
        assert_eq!(e, &format!("append {i}"));
    }
    assert_eq!(events[appends + 1], "competitor", "{events:?}");
    let local = Arc::into_inner(local).unwrap();
    local.close().unwrap();
}

#[test]
fn hpb17_limits_precede_materialization_and_asset_reads() {
    let dir = tempfile::tempdir().unwrap();
    let (r, profile, _, v1) = source(&dir.path().join("src"));
    let l1 = landing_of(&r, v1);
    let bytes = export(&r, v1);
    let blocks = r
        .read()
        .materialized(&Scope::At(l1))
        .unwrap()
        .iter()
        .count();
    let (root, mut h) = holder(dir.path(), repo(&r));
    let before = state(&root, &h);
    let len = figure().len() as u64;
    let at_bound = Limits {
        selection: blocks,
        blob: len,
        assets: len,
        ..Limits::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let counts = || (materializations_for_tests(), blob_payload_reads_for_tests());

    // one past each bound: the take's own checks refuse before the root landing is
    // materialized, before the verdict, and before any blob payload is read for the facts
    for (case, limits, message) in [
        (
            "selection",
            Limits {
                selection: blocks - 1,
                ..at_bound
            },
            format!(
                "Publication selection exceeds the limit of {} blocks.",
                blocks - 1
            ),
        ),
        (
            "blob",
            Limits {
                blob: len - 1,
                ..at_bound
            },
            format!("Publication blob exceeds the limit of {} bytes.", len - 1),
        ),
        (
            "assets",
            Limits {
                assets: len - 1,
                ..at_bound
            },
            format!("Publication assets exceeds the limit of {} bytes.", len - 1),
        ),
    ] {
        let start = counts();
        let guard = publication::override_limits_for_tests(limits);
        let got = take_publication(
            &mut h,
            &files(dir.path(), case, &bytes),
            target("v1.0.0", l1, &profile),
            counting(&calls),
        );
        drop(guard);
        let e = core(got);
        assert_eq!((e.code, e.message.as_str()), (E_LIMIT, message.as_str()));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(counts(), start, "{case}: nothing materialized or read");
        assert_eq!(state(&root, &h), before, "{case}");
    }

    // at each bound the take materializes once, calls the verdict once and reads no payload
    let start = counts();
    let blob_reads = publication::blob_reads_for_tests();
    let guard = publication::override_limits_for_tests(at_bound);
    let got = take_publication(
        &mut h,
        &files(dir.path(), "ok", &bytes),
        target("v1.0.0", l1, &profile),
        counting(&calls),
    );
    drop(guard);
    let got = got.unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(counts(), (start.0 + 1, start.1));
    assert_eq!(publication::blob_reads_for_tests(), blob_reads);
    let facts = got.facts.profile.expect("the profile");
    assert_eq!(facts.references, CheckStatus::Clear);
    assert_eq!(
        facts.selection.len(),
        3,
        "the doc, its prose and its figure"
    );
}
