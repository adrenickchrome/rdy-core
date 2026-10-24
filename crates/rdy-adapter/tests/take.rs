//! ha16 (D-D2NPN, D-BPYN8, D-WDKWQ, D-CC90E): the take of one upload through the adapter. The
//! adapter clones the hub's file handles and hands them, with the publication target, to the
//! engine's take under the store's one writer; everything else is core's. So every reply here is
//! compared with the core take it stands for, and the hub's files stay the hub's.
//!
//! The source projects are built with core calls and closed before an adapter handle opens a
//! store.

use rdy_adapter::stream::ObjectKey;
use rdy_adapter::take::{StorePosition, TakeOutcome, UploadTarget};
use rdy_adapter::{Hash, RdyError, RepoId, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export, stream_take};
use rdy_model::{Provenance, Role};
use rdy_store::LandRequest;
use rdy_store::kinds;
use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

const CANARY: &str = "ZEBRA-7731";

// ---------------------------------------------------------------------------------------------
// source projects (core)

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

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ha16".into(),
        },
    )
    .unwrap()
}

/// Proposes view `work` and lands it; returns the landing.
fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
}

/// Tags canon's head `name`; returns the tag's hash.
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

/// A project at `dir` with a doc and a prose block that holds the canary, landed. Returns the
/// project, the doc's id and the prose block's id.
fn project(dir: &Path) -> (Rdy, String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("The launch code is {CANARY}.")}
        ]),
    );
    land(&mut r, "first");
    (r, w.created["$d"].clone(), w.created["$p"].clone())
}

/// Retitles the doc `doc` and lands it; returns the landing.
fn retitle(r: &mut Rdy, doc: &str, title: &str) -> Hash {
    write(
        r,
        json!([{"op": "update", "id": doc, "fields": {"title": title}}]),
    );
    land(r, title)
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
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

/// The whole history of `tag` as one batch, as core's export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// The keys core's export of `tag` holds.
fn export_keys(r: &Rdy, tag: Hash) -> Vec<ObjectKey> {
    stream_export::write(&r.read(), tag, &BTreeSet::new(), std::io::sink())
        .unwrap()
        .keys
}

/// `batches` written as the hub's files under `dir`, with names from `name`.
fn paths(dir: &Path, name: &str, batches: &[&Vec<u8>]) -> Vec<PathBuf> {
    batches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = dir.join(format!("{name}-{i}.rdysync"));
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect()
}

fn open_all(paths: &[PathBuf]) -> Vec<File> {
    paths.iter().map(|p| File::open(p).unwrap()).collect()
}

fn files(dir: &Path, name: &str, batches: &[&Vec<u8>]) -> Vec<File> {
    open_all(&paths(dir, name, batches))
}

fn target(tag: &str, landing: Hash) -> UploadTarget {
    UploadTarget {
        tag: tag.into(),
        landing,
    }
}

fn segments_hold_canary(root: &Path) -> bool {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
        .any(|e| {
            let b = std::fs::read(e.path()).unwrap();
            b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes())
        })
}

fn refusal(e: &RdyError) -> (String, String, String, String) {
    (
        e.code.to_string(),
        e.message.clone(),
        e.fix.clone(),
        format!("{:?}", e.issues),
    )
}

/// A source project with the canary landed and tagged `v1.0.0`, then removed and tagged
/// `v2.0.0`. Returns the source (still open), the prose id, both tags and the export of
/// `v1.0.0` made before the removal (an export made after it carries the removal too).
fn removed_source(dir: &Path) -> (Rdy, String, Hash, Hash, Vec<u8>) {
    let (mut r, _, secret) = project(&dir.join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    let before = export(&r, v1);
    remove(&mut r, &secret);
    let v2 = tag(&mut r, "v2.0.0");
    (r, secret, v1, v2, before)
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ha16_adapter_take_matches_core_outcome() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, _, v1, v2, before) = removed_source(dir.path());
    let id = repo(&r);
    let uploads = [
        ("v1.0.0", landing_of(&r, v1), before),
        ("v2.0.0", landing_of(&r, v2), export(&r, v2)),
    ];

    // the same uploads into two holders of the repo: one by core, one through the adapter
    let mut core = Rdy::create_holder(&dir.path().join("core"), id).unwrap();
    let mut handle = StoreHandle::create_holder(&dir.path().join("adapter"), id).unwrap();
    for (round, (name, l, bytes)) in uploads.iter().enumerate() {
        for _ in 0..2 {
            let want = stream_take::take(
                &mut core,
                &files(dir.path(), &format!("c{round}"), &[bytes]),
                target(name, *l),
            )
            .unwrap();
            let got: TakeOutcome = handle
                .take(
                    &files(dir.path(), &format!("a{round}"), &[bytes]),
                    target(name, *l),
                )
                .unwrap();
            // the owned outcome, as core gives it, and the same JSON reply
            assert_eq!(got, want, "{name}");
            assert_eq!(
                serde_json::to_vec(&got).unwrap(),
                serde_json::to_vec(&want).unwrap()
            );
        }
    }
    let last = handle
        .take(
            &files(dir.path(), "again", &[&uploads[1].2]),
            target("v2.0.0", uploads[1].1),
        )
        .unwrap();
    assert!(last.unchanged && last.canon.is_none(), "{last:?}");
    let _: StorePosition = last.end;

    // refusals are core's, code, text, fix and issues
    let refused =
        |h: &mut StoreHandle, core: &mut Rdy, fs: &dyn Fn() -> Vec<File>, t: UploadTarget| {
            let a = h.take(&fs(), t.clone()).unwrap_err();
            let c = stream_take::take(core, &fs(), t).unwrap_err();
            assert_eq!(refusal(&a), refusal(&c));
            a
        };
    let fs = || files(dir.path(), "v1", &[&uploads[0].2]);
    let e = refused(&mut handle, &mut core, &fs, target("v1.0.0", uploads[1].1));
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_RECORD",
            "upload target does not match the batch root tag"
        )
    );
    let none = || Vec::new();
    let e = refused(
        &mut handle,
        &mut core,
        &none,
        target("v1.0.0", uploads[0].1),
    );
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_RECORD", "stream record 0 at byte 0: no batch files")
    );
    let long = target(&"v".repeat(1_048_577), uploads[0].1);
    let e = refused(&mut handle, &mut core, &fs, long);
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "upload tag exceeds 1048576 bytes")
    );
    handle.close().unwrap();
    core.close().unwrap();

    // a project is no holder: refused, as core refuses it
    r.close().unwrap();
    let src = dir.path().join("src");
    let mut project = StoreHandle::open(&src).unwrap();
    let e = project
        .take(
            &files(dir.path(), "p", &[&uploads[0].2]),
            target("v1.0.0", uploads[0].1),
        )
        .unwrap_err();
    project.close().unwrap();
    let mut core = Rdy::open(&src).unwrap();
    let c = stream_take::take(
        &mut core,
        &files(dir.path(), "p", &[&uploads[0].2]),
        target("v1.0.0", uploads[0].1),
    )
    .unwrap_err();
    assert_eq!(refusal(&e), refusal(&c));
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "this store is a project",
            "open it through the project adapter"
        )
    );
}

/// One upload the caller queues: its name, files and target.
type Job = (String, Vec<File>, UploadTarget);
/// What the writer thread answers for one job: the name, the take's result and the tag names a
/// snapshot taken right after it reads.
type Reply = (String, Result<TakeOutcome, RdyError>, Vec<String>);

#[test]
fn ha16_writes_publish_in_caller_order() {
    let dir = tempfile::tempdir().unwrap();
    let (mut r, doc, _) = project(&dir.path().join("src"));
    let v1 = tag(&mut r, "v1.0.0");
    retitle(&mut r, &doc, "Two");
    let v2 = tag(&mut r, "v2.0.0");
    retitle(&mut r, &doc, "Three");
    let v3 = tag(&mut r, "v3.0.0");
    let id = repo(&r);
    let up = |name: &str, t: Hash| -> Job {
        (
            name.to_string(),
            files(dir.path(), name, &[&export(&r, t)]),
            target(name, landing_of(&r, t)),
        )
    };
    // the caller's order, not version order; one refusal among them
    let jobs = vec![
        up("v2.0.0", v2),
        (
            "bad".to_string(),
            files(dir.path(), "bad", &[&export(&r, v3)]),
            target("v3.0.0", landing_of(&r, v1)),
        ),
        up("v1.0.0", v1),
        up("v3.0.0", v3),
    ];

    // the hub's queue: one thread owns the handle, takes jobs in the order they arrive and
    // answers each before it reads the next
    let root = dir.path().join("holder");
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (reply_tx, reply_rx) = mpsc::channel::<Reply>();
    let worker = std::thread::spawn(move || {
        let mut handle = StoreHandle::create_holder(&root, id).unwrap();
        for (name, fs, t) in job_rx {
            let got = handle.take(&fs, t);
            let tags = handle
                .snapshot()
                .unwrap()
                .tags()
                .unwrap()
                .into_iter()
                .map(|(_, t)| t.name)
                .collect();
            reply_tx.send((name, got, tags)).unwrap();
        }
        handle.close().unwrap();
    });
    for job in jobs {
        job_tx.send(job).unwrap();
    }
    drop(job_tx);
    let replies: Vec<Reply> = reply_rx.iter().collect();
    worker.join().unwrap();

    let names: Vec<&str> = replies.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(names, ["v2.0.0", "bad", "v1.0.0", "v3.0.0"]);
    // each take is published before the next one starts; the log only grows
    let tags: Vec<Vec<String>> = replies.iter().map(|(_, _, t)| t.clone()).collect();
    assert_eq!(
        tags,
        [
            vec!["v2.0.0"],
            vec!["v2.0.0"],
            vec!["v1.0.0", "v2.0.0"],
            vec!["v1.0.0", "v2.0.0", "v3.0.0"],
        ]
    );
    assert_eq!(replies[1].1.as_ref().unwrap_err().code, "E_BAD_RECORD");
    let ends: Vec<(u32, u64)> = [0, 2, 3]
        .iter()
        .map(|&i| {
            let o = replies[i].1.as_ref().unwrap();
            (o.end.segment, o.end.offset)
        })
        .collect();
    assert!(ends.windows(2).all(|w| w[0] < w[1]), "{ends:?}");
    // v1.0.0 came after v2.0.0's history: only its tag object was new
    assert_eq!(replies[2].1.as_ref().unwrap().records_taken, 1);
}

#[test]
fn ha16_snapshot_refs_and_removal_epoch_are_correct() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, secret, v1, v2, before) = removed_source(dir.path());
    let l1 = landing_of(&r, v1);
    let mut handle = StoreHandle::create_holder(&dir.path().join("holder"), repo(&r)).unwrap();
    let keys1 = rdy_adapter::stream::check_batch(before.as_slice(), None)
        .unwrap()
        .keys;

    // a snapshot from before a take keeps its refs and its log end
    let empty = handle.snapshot().unwrap();
    handle
        .take(&files(dir.path(), "v1", &[&before]), target("v1.0.0", l1))
        .unwrap();
    assert!(empty.tags().unwrap().is_empty());
    assert!(empty.canon().is_none());
    assert!(
        empty
            .has_objects(&keys1)
            .unwrap()
            .iter()
            .all(|p| !p.present)
    );
    assert_eq!(empty.tag_history("v1.0.0").unwrap_err().code, "E_NOT_FOUND");

    // a new snapshot includes the take
    let first = handle.snapshot().unwrap();
    assert_eq!(
        first.tags().unwrap(),
        vec![(v1, r.store().read_tag(&v1).unwrap())]
    );
    assert!(
        first
            .has_objects(&keys1)
            .unwrap()
            .iter()
            .all(|p| p.present && !p.redacted)
    );
    let facts = first.landing_facts(l1).unwrap();
    let secret_key = ObjectKey {
        kind: kinds::BLOCK,
        hash: facts
            .blocks
            .iter()
            .find(|b| b.id.to_string() == secret)
            .unwrap()
            .version
            .unwrap(),
    };

    // a take that imports a removal: the older snapshots keep their captured refs, and every
    // content read of theirs is refused by the existing epoch guard
    handle
        .take(
            &files(dir.path(), "v2", &[&export(&r, v2)]),
            target("v2.0.0", landing_of(&r, v2)),
        )
        .unwrap();
    for (old, held) in [(&empty, 0), (&first, 1)] {
        assert_eq!(old.tags().unwrap().len(), held);
        for e in [
            old.has_objects(&keys1).unwrap_err(),
            old.landing_facts(l1).unwrap_err(),
        ] {
            assert_eq!(e.code, "E_REDACTED_STATE", "{e}");
        }
    }
    assert_eq!(
        first.tag_history("v1.0.0").unwrap_err().code,
        "E_REDACTED_STATE"
    );

    // a snapshot taken after it reads both tags and the removed object as present and redacted
    let after = handle.snapshot().unwrap();
    let names: Vec<String> = after
        .tags()
        .unwrap()
        .into_iter()
        .map(|(_, t)| t.name)
        .collect();
    assert_eq!(names, ["v1.0.0", "v2.0.0"]);
    let p = after.has_objects(&[secret_key]).unwrap()[0];
    assert_eq!((p.present, p.redacted), (true, true));
    let w = after
        .landing_facts(l1)
        .unwrap()
        .blocks
        .into_iter()
        .find(|b| b.version == Some(secret_key.hash))
        .unwrap();
    assert!(w.redacted);
    assert!(after.canon().is_none() && after.canon_chain().unwrap().is_empty());
    handle.close().unwrap();
    assert!(!segments_hold_canary(&dir.path().join("holder")));
}

#[test]
fn ha16_take_preserves_hub_files_after_success_and_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _, v1, _, bytes) = removed_source(dir.path());
    let l1 = landing_of(&r, v1);
    let (head, tail) = bytes.split_at(bytes.len() / 2);
    let hub = dir.path().join("quarantine");
    std::fs::create_dir_all(&hub).unwrap();
    let ps = paths(&hub, "up", &[&bytes]);
    let root = dir.path().join("holder");
    let mut handle = StoreHandle::create_holder(&root, repo(&r)).unwrap();
    let on_disk = |p: &PathBuf| std::fs::read(p).unwrap();

    // a refused take: the hub's file is there, byte for byte, and its handle still reads
    let fs = open_all(&ps);
    let e = handle.take(&fs, target("v9.0.0", l1)).unwrap_err();
    assert_eq!(e.code, "E_BAD_RECORD");
    assert_eq!(on_disk(&ps[0]), bytes);
    assert!(handle.snapshot().unwrap().tags().unwrap().is_empty());

    // the caller's cursor is not the take's: a handle moved forward still takes from its start
    let mut fs = fs;
    fs[0].seek(SeekFrom::Start(17)).unwrap();
    // a file cut after its check is refused before anything is sealed
    std::fs::write(&ps[0], head).unwrap();
    let cut = handle
        .take(&open_all(&ps), target("v1.0.0", l1))
        .unwrap_err();
    assert!(
        ["E_STREAM_TRUNCATED", "E_BAD_RECORD"].contains(&cut.code),
        "{cut:?}"
    );
    assert!(handle.snapshot().unwrap().tags().unwrap().is_empty());
    std::fs::write(&ps[0], [head, tail].concat()).unwrap();
    let got = handle.take(&fs, target("v1.0.0", l1)).unwrap();
    assert_eq!(got.tag_hash, v1);

    // a successful take leaves the file to the hub, and the caller's handle still reads it all
    assert_eq!(on_disk(&ps[0]), bytes);
    let mut again = Vec::new();
    fs[0].seek(SeekFrom::Start(0)).unwrap();
    fs[0].read_to_end(&mut again).unwrap();
    assert_eq!(again, bytes);
    drop(fs);
    assert_eq!(
        std::fs::read_dir(&hub).unwrap().count(),
        1,
        "core removes no hub file"
    );

    handle.close().unwrap();
    assert_eq!(on_disk(&ps[0]), bytes);
}

#[test]
fn ha16_failed_take_repair_reads_match_the_sealed_log() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, secret, v1, v2, before) = removed_source(dir.path());
    let (l1, l2) = (landing_of(&r, v1), landing_of(&r, v2));
    let root = dir.path().join("holder");
    let mut handle = StoreHandle::create_holder(&root, repo(&r)).unwrap();
    let f1 = files(dir.path(), "v1", &[&before]);
    let f2 = files(dir.path(), "v2", &[&export(&r, v2)]);
    handle.take(&f1, target("v1.0.0", l1)).unwrap();
    assert!(segments_hold_canary(&root));

    // the take seals its removal, then fails before the erasure finishes: no success reply
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let e = handle.take(&f2, target("v2.0.0", l2)).unwrap_err();
    arm_failpoint(None);
    assert_eq!(e.code, "E_REDACTION_PENDING", "{e:?}");
    drop(handle);

    // the hub does not know whether the upload landed: it reads the holder again
    let mut handle = StoreHandle::open_holder(&root).unwrap();
    let snap = handle.snapshot().unwrap();
    let tags = snap.tags().unwrap();
    assert_eq!(
        tags.iter()
            .map(|(h, t)| (*h, t.name.as_str()))
            .collect::<Vec<_>>(),
        [(v1, "v1.0.0"), (v2, "v2.0.0")]
    );
    let keys = export_keys(&r, v2);
    let seen = snap.has_objects(&keys).unwrap();
    assert!(seen.iter().all(|p| p.present), "{seen:?}");
    assert!(seen.iter().any(|p| p.redacted));
    // the reopen finished the erasure
    assert!(!segments_hold_canary(&root));
    let history: Vec<Hash> = snap
        .tag_history("v2.0.0")
        .unwrap()
        .iter()
        .map(|f| f.hash)
        .collect();
    let facts: Vec<Hash> = rdy_engine::history_facts::chain(&r.read(), l2)
        .unwrap()
        .iter()
        .map(|f| f.hash)
        .collect();
    assert_eq!(history, facts);
    assert!(
        snap.landing_facts(l1)
            .unwrap()
            .blocks
            .iter()
            .any(|b| b.id.to_string() == secret && b.redacted)
    );
    handle.fsck().unwrap();

    // a retry of either upload appends nothing and names the held tag
    for (fs, name, l, t) in [(&f2, "v2.0.0", l2, v2), (&f1, "v1.0.0", l1, v1)] {
        let again = handle.take(fs, target(name, l)).unwrap();
        assert_eq!(
            (
                again.unchanged,
                again.frames_written,
                again.records_taken,
                again.tag_hash
            ),
            (true, 0, 0, t)
        );
    }
    handle.close().unwrap();
}

// ---------------------------------------------------------------------------------------------
// rf4: store format 18 (D-4WC6R): a history with both Reflects forms

/// Rewrites block `b` in view `work` with `body`, justified by `j`, as one typed commit; returns
/// the change.
fn reflects_rewrite(r: &mut Rdy, b: &str, body: &str, j: rdy_model::Justification) -> Hash {
    let scope = rdy_engine::Scope::View("work".into());
    let mut next = r
        .read()
        .block(&scope, &rdy_model::BlockId::parse(b).unwrap())
        .unwrap();
    let base = next.version_hash();
    next.body = body.into();
    let deps = r.read().included("work").to_vec();
    let h = r
        .store_mut()
        .commit_to_view(
            "work",
            rdy_model::Change {
                view: "work".into(),
                ops: vec![rdy_model::Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: "agent:a".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: 7,
                },
                justification: Some(j),
                message: "rf4".into(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    h
}

/// A source project with a doc, a source block and a claim, landed; then a source-only and a
/// code-only Reflects rewrite of the claim, landed and tagged `v1.0.0`. Returns the source, its
/// tag, the two changes and their justifications.
fn reflects_source(dir: &Path) -> (Rdy, Hash, Vec<(Hash, rdy_model::Justification)>) {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "source", "id": "$s", "parent": "$d", "fields": {"title": "The spec", "kind": "spec", "locator": "spec/v1.md"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$d", "body": "One."}
        ]),
    );
    land(&mut r, "first");
    let source = rdy_model::BlockId::parse(&w.created["$s"]).unwrap();
    let claim = w.created["$c"].clone();
    let forms = [
        rdy_model::Justification::Reflects {
            sources: vec![source],
            code: false,
        },
        rdy_model::Justification::Reflects {
            sources: vec![],
            code: true,
        },
    ];
    let changes = forms
        .into_iter()
        .zip(["Two.", "Three."])
        .map(|(j, body)| (reflects_rewrite(&mut r, &claim, body, j.clone()), j))
        .collect();
    land(&mut r, "reflects");
    let t = tag(&mut r, "v1.0.0");
    (r, t, changes)
}

#[test]
fn rf4_adapter_check_take_and_history_decode_reflects() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let (r, t, changes) = reflects_source(&dir.path().join("src"));
    let bytes = export(&r, t);
    let repo = repo(&r);
    let landing = landing_of(&r, t);
    let history: Vec<Hash> = rdy_engine::history_facts::chain(&r.read(), landing)
        .unwrap()
        .into_iter()
        .map(|f| f.hash)
        .collect();
    r.close().unwrap();
    // the check: both Reflects changes are keys of the batch
    let report = rdy_adapter::stream::check_batch(bytes.as_slice(), Some(repo)).unwrap();
    for (c, _) in &changes {
        assert!(report.keys.contains(&ObjectKey {
            kind: kinds::CHANGE,
            hash: *c
        }));
    }
    // the take into a new holder
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo).unwrap();
    let got = h
        .take(
            &files(dir.path(), "rf4", &[&bytes]),
            target("v1.0.0", landing),
        )
        .unwrap();
    assert_eq!(got.records_taken, report.keys.len() as u64);
    // the history through the snapshot names both changes, and each is present
    let snap = h.snapshot().unwrap();
    let facts = snap.tag_history("v1.0.0").unwrap();
    assert_eq!(facts.iter().map(|f| f.hash).collect::<Vec<_>>(), history);
    let written: BTreeSet<Hash> = facts
        .iter()
        .flat_map(|f| f.blocks.iter().map(|b| b.change))
        .collect();
    let keys: Vec<ObjectKey> = changes
        .iter()
        .map(|(c, _)| ObjectKey {
            kind: kinds::CHANGE,
            hash: *c,
        })
        .collect();
    for k in &keys {
        assert!(written.contains(&k.hash));
    }
    assert!(
        snap.has_objects(&keys)
            .unwrap()
            .iter()
            .all(|p| p.present && !p.redacted)
    );
    drop(snap);
    let fsck = serde_json::to_value(h.fsck().unwrap()).unwrap();
    // a dump through the handle loads to the same holder
    let out = dir.path().join("holder.rdydump");
    h.dump(&out).unwrap();
    h.close().unwrap();
    let copy = dir.path().join("copy");
    let loaded = rdy_adapter::maintenance::load(&out, &copy).unwrap();
    assert_eq!(serde_json::to_value(&loaded.fsck).unwrap(), fsck);
    // each holder decodes both justifications under their hashes, and a reindex and fsck agree
    for at in [&root, &copy] {
        let mut hr = Rdy::open_holder(at).unwrap();
        for (c, j) in &changes {
            let got = hr.store().change(c).unwrap();
            assert_eq!(got.justification.as_ref(), Some(j));
            assert_eq!(got.id(), *c);
        }
        let before = hr.store().index_dump_for_tests().unwrap();
        hr.reindex().unwrap();
        assert_eq!(hr.store().index_dump_for_tests().unwrap(), before);
        assert_eq!(serde_json::to_value(hr.fsck().unwrap()).unwrap(), fsck);
        hr.close().unwrap();
    }
}
