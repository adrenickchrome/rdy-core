//! scc2 (D-KE83X, D-3RSCW): a project's fetch of a canon or view history. One batch takes the
//! records, the free tag refs and the remote's sync state; no canon or view ref moves. A retry
//! keeps one copy; a fetched tag never replaces a local tag of its name; a failed batch keeps
//! the applied cursor; the state is used only for the hub repo it names.
use rdy_engine::stream_export;
use rdy_engine::sync_fetch::{self, FetchOutcome, HubState, usable_canon_state, usable_view_state};
use rdy_engine::*;
use rdy_model::*;
use rdy_store::format::{FEATURE_SYNC_STATE, RdyVersion};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{SyncCanon, SyncStep};
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

const URL: &str = "https://127.0.0.1:9/r/demo";
const MOVED: &str = "https://127.0.0.1:10/r/demo";

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

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "scc2".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) -> Hash {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap()
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

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The source project after its first landing, a copy of it as the receiver, then two more
/// landings in the source, the last tagged `v2.0.0`. Returns the source, the receiver and the
/// three landings.
struct Pair {
    dir: tempfile::TempDir,
    src: Rdy,
    dst: Rdy,
    ls: [Hash; 3],
}

fn pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let mut src = Rdy::init(&a, "rdy-standard").unwrap();
    let w = write(
        &mut src,
        "work",
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let d = w.created["$d"].clone();
    let l1 = land(&mut src, "first");
    drop(src);
    let b = dir.path().join("b");
    copy_dir(&a, &b);
    let dst = Rdy::open(&b).unwrap();
    let mut src = Rdy::open(&a).unwrap();
    write(
        &mut src,
        "work",
        json!([{"op": "update", "id": d, "fields": {"title": "A2"}}]),
    );
    let l2 = land(&mut src, "second");
    write(
        &mut src,
        "work",
        json!([{"op": "update", "id": d, "fields": {"title": "A3"}}]),
    );
    let l3 = land(&mut src, "third");
    tag(&mut src, "v2.0.0");
    Pair {
        dir,
        src,
        dst,
        ls: [l1, l2, l3],
    }
}

/// The source's canon history from `root` above `base`, as one batch file.
fn canon_batch(p: &Pair, root: Hash, base: Option<Hash>, name: &str) -> PathBuf {
    let mut out = Vec::new();
    stream_export::write_history(
        &p.src.read(),
        StreamPurpose::CanonHistory,
        root,
        base,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    let path = p.dir.path().join(name);
    std::fs::write(&path, out).unwrap();
    path
}

fn target(root: Hash, url: &str) -> FetchTarget {
    FetchTarget {
        remote: "origin".into(),
        url: url.into(),
        purpose: StreamPurpose::CanonHistory,
        root,
        view: None,
    }
}

fn fetch(r: &mut Rdy, files: &[&PathBuf], t: &FetchTarget) -> Result<FetchOutcome, RdyError> {
    let opened: Vec<File> = files.iter().map(|p| File::open(p).unwrap()).collect();
    sync_fetch::fetch(r, &opened, t)
}

fn has(r: &Rdy, kind: u8, h: Hash) -> bool {
    r.store().reader().has_kind(kind, &h).unwrap()
}

fn repo(r: &Rdy) -> RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

#[test]
fn scc2_fetch_is_one_batch_without_canon_or_view_moves() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, l2, l3] = p.ls;
    let canon_before = p.dst.store().reader().canon_head().unwrap();
    assert_eq!(canon_before, Some(l1));
    let work_before = p.dst.store().reader().view_head("work").unwrap();
    let writes = p.dst.store().write_count();
    let file = canon_batch(&p, l3, Some(l1), "c");
    let got = fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert!(got.records_taken > 0 && got.frames_written > got.records_taken);
    assert_eq!(got.tags_taken, ["v2.0.0"]);
    assert!(got.tags_kept.is_empty());
    assert_eq!(got.min_rdy_before, Some(RdyVersion(0, 52, 0)));
    assert_eq!(got.min_rdy_after, Some(RdyVersion(0, 53, 0)));
    assert!(!got.unchanged);
    assert_eq!(p.dst.store().write_count(), writes + 1, "one batch");
    // the records are held; canon and the views did not move
    for l in [l2, l3] {
        assert!(has(&p.dst, kinds::LANDING, l));
    }
    let reader = p.dst.store().reader();
    assert_eq!(reader.canon_head().unwrap(), Some(l1));
    assert_eq!(reader.view_head("work").unwrap(), work_before);
    // the tag ref and the sync state came in the same batch, the feature with them
    assert!(reader.get_ref("tag/v2.0.0").unwrap().is_some());
    assert_eq!(
        reader.sync_canon("origin").unwrap(),
        Some(SyncCanon {
            repo_id: repo(&p.src),
            url: URL.into(),
            fetched: Some(l3),
            applied: None,
            held: None,
        })
    );
    assert!(p.dst.store().features().has(FEATURE_SYNC_STATE));
    p.dst.store().check_objects().unwrap();
    drop(reader);

    // a view history: the view state names the chain's first op; no view ref is written
    let ops = {
        let read = p.src.read();
        let reader = read.reader();
        let mut ops = vec![];
        let mut at = Some(reader.view_head("work").unwrap().unwrap().head);
        while let Some(h) = at {
            ops.push(h);
            at = reader.view_op(&h).unwrap().prev;
        }
        ops.reverse();
        ops
    };
    let head = *ops.last().unwrap();
    let mut out = Vec::new();
    stream_export::write_history(
        &p.src.read(),
        StreamPurpose::ViewHistory,
        head,
        None,
        &BTreeSet::from([ObjectKey {
            kind: kinds::LANDING,
            hash: l3,
        }]),
        &mut out,
    )
    .unwrap();
    let vfile = p.dir.path().join("v");
    std::fs::write(&vfile, out).unwrap();
    let vt = FetchTarget {
        purpose: StreamPurpose::ViewHistory,
        root: head,
        view: Some("work".into()),
        ..target(head, URL)
    };
    fetch(&mut p.dst, &[&vfile], &vt).unwrap();
    let reader = p.dst.store().reader();
    assert_eq!(reader.view_head("work").unwrap(), work_before);
    assert_eq!(reader.canon_head().unwrap(), Some(l1));
    let v = reader.sync_view("origin", "work").unwrap().unwrap();
    assert_eq!((v.root, v.fetched, v.applied), (ops[0], Some(head), None));
    p.dst.store().check_objects().unwrap();
}

#[test]
fn scc2_fetch_retry_keeps_one_copy() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    let file = canon_batch(&p, l3, Some(l1), "c");
    // one file given twice is one copy
    let first = fetch(&mut p.dst, &[&file, &file], &target(l3, URL)).unwrap();
    let end = p.dst.store().log_end();
    let writes = p.dst.store().write_count();
    let again = fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert!(again.unchanged);
    assert_eq!((again.records_taken, again.frames_written), (0, 0));
    assert!(again.tags_taken.is_empty() && again.tags_kept.is_empty());
    assert_eq!(again.min_rdy_before, first.min_rdy_after);
    assert_eq!(p.dst.store().log_end(), end);
    assert_eq!(p.dst.store().write_count(), writes);
    p.dst.store().check_objects().unwrap();
}

#[test]
fn scc2_fetch_tag_collision_keeps_local_tag() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    // the receiver names its own first landing v2.0.0
    let local = tag(&mut p.dst, "v2.0.0");
    let theirs = tag_hash(&p.src, "v2.0.0");
    assert_ne!(local, theirs);
    let file = canon_batch(&p, l3, Some(l1), "c");
    let got = fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert!(got.tags_taken.is_empty());
    assert_eq!(got.tags_kept, ["v2.0.0"]);
    let reader = p.dst.store().reader();
    assert_eq!(
        reader.get_ref("tag/v2.0.0").unwrap(),
        Some(local.0.to_vec()),
        "the local tag stays"
    );
    // the fetched tag object is held, unnamed
    assert!(has(&p.dst, kinds::TAG, theirs));
    p.dst.store().check_objects().unwrap();
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

#[test]
fn scc2_fetch_crash_preserves_applied_cursor() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    let r = repo(&p.src);
    let applied = SyncCanon {
        repo_id: r,
        url: URL.into(),
        fetched: Some(l1),
        applied: Some(l1),
        held: None,
    };
    p.dst
        .store_mut()
        .write_sync("origin", vec![SyncStep::Canon(applied.clone())])
        .unwrap();
    let file = canon_batch(&p, l3, Some(l1), "c");
    // the batch fails after its frames are appended: nothing of it stays
    let end = p.dst.store().log_end();
    p.dst.store_mut().fail_next_index_apply();
    assert!(fetch(&mut p.dst, &[&file], &target(l3, URL)).is_err());
    assert_eq!(p.dst.store().log_end(), end);
    assert_eq!(
        p.dst.store().reader().sync_canon("origin").unwrap(),
        Some(applied.clone())
    );
    assert!(!has(&p.dst, kinds::LANDING, l3));
    // a reopen reads the same cursor
    let path = p.dir.path().join("b");
    drop(p.dst);
    p.dst = Rdy::open(&path).unwrap();
    assert_eq!(
        p.dst.store().reader().sync_canon("origin").unwrap(),
        Some(applied.clone())
    );
    // the fetch that seals moves only `fetched`, from a moved URL too
    fetch(&mut p.dst, &[&file], &target(l3, MOVED)).unwrap();
    assert_eq!(
        p.dst.store().reader().sync_canon("origin").unwrap(),
        Some(SyncCanon {
            url: MOVED.into(),
            fetched: Some(l3),
            ..applied
        })
    );
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), Some(l1));
}

#[test]
fn scc2_sync_state_uses_only_matching_remote_repo() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    let r = repo(&p.src);
    let other = RepoId([9; 16]);
    let file = canon_batch(&p, l3, Some(l1), "c");
    fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    let op = p
        .dst
        .store()
        .reader()
        .view_head("work")
        .unwrap()
        .unwrap()
        .head;
    p.dst
        .store_mut()
        .write_sync(
            "origin",
            vec![SyncStep::View {
                name: "work".into(),
                value: rdy_store::sync_state::SyncView {
                    repo_id: r,
                    url: URL.into(),
                    root: op,
                    fetched: Some(op),
                    applied: Some(op),
                },
            }],
        )
        .unwrap();
    let end = p.dst.store().log_end();
    let canon = p.dst.store().reader().canon_head().unwrap();
    let reader = p.dst.store().reader();
    let hub = |repo_id, canon| HubState { repo_id, canon };
    // the reply's repo is the state's: used, whatever URL the remote has now
    let state = usable_canon_state(&reader, "origin", &hub(Some(r), Some(l3))).unwrap();
    assert_eq!(state.map(|s| s.fetched), Some(Some(l3)));
    assert!(
        usable_view_state(&reader, "origin", "work", &hub(Some(r), None))
            .unwrap()
            .is_some()
    );
    // a null canon, a null repo id or another repo id: not used
    for h in [
        hub(Some(r), None),
        hub(None, Some(l3)),
        hub(Some(other), Some(l3)),
    ] {
        assert_eq!(
            usable_canon_state(&reader, "origin", &h).unwrap(),
            None,
            "{h:?}"
        );
    }
    for h in [hub(None, Some(l3)), hub(Some(other), Some(l3))] {
        assert_eq!(
            usable_view_state(&reader, "origin", "work", &h).unwrap(),
            None,
            "{h:?}"
        );
    }
    // another remote name reads only its own refs
    assert_eq!(
        usable_canon_state(&reader, "backup", &hub(Some(r), Some(l3))).unwrap(),
        None
    );
    assert_eq!(
        usable_view_state(&reader, "backup", "work", &hub(Some(r), Some(l3))).unwrap(),
        None
    );
    drop(reader);
    // a remote removed and added again under the name finds the refs, used only for its repo
    p.dst
        .store_mut()
        .write_sync(
            "backup",
            vec![SyncStep::Canon(SyncCanon {
                repo_id: other,
                url: MOVED.into(),
                fetched: None,
                applied: None,
                held: None,
            })],
        )
        .unwrap();
    let reader = p.dst.store().reader();
    assert!(
        usable_canon_state(&reader, "backup", &hub(Some(other), Some(l3)))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        usable_canon_state(&reader, "backup", &hub(Some(r), Some(l3))).unwrap(),
        None
    );
    assert!(
        usable_canon_state(&reader, "origin", &hub(Some(r), Some(l3)))
            .unwrap()
            .is_some(),
        "one remote's write leaves the other's state"
    );
    drop(reader);
    // the state check itself removes no landing and writes nothing
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), canon);
    assert!(has(&p.dst, kinds::LANDING, l1) && has(&p.dst, kinds::LANDING, l3));
    let end_after_backup = p.dst.store().log_end();
    assert!(end_after_backup > end);
    let reader = p.dst.store().reader();
    for _ in 0..2 {
        usable_canon_state(&reader, "origin", &hub(None, None)).unwrap();
        usable_canon_state(&reader, "origin", &hub(Some(other), Some(l3))).unwrap();
    }
    drop(reader);
    assert_eq!(p.dst.store().log_end(), end_after_backup);
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), canon);
}

/// The sync fixtures directory of the QA crate.
fn sync_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures/sync")
}

/// One format-4 fixture: its batch from the source, its check report, and its fetch into the
/// receiver (a copy of the source at its first landing), as `<name>.bin` and `<name>.expect.json`.
fn write_fixture(p: &mut Pair, name: &str, purpose: StreamPurpose, root: Hash, view: Option<&str>) {
    let mut out = Vec::new();
    let report = stream_export::write_history(
        &p.src.read(),
        purpose,
        root,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    let checked = rdy_engine::stream_check::check_batch(&out[..], None).unwrap();
    assert_eq!(checked, report);
    let bin = sync_fixtures().join(format!("{name}.bin"));
    let expect = sync_fixtures().join(format!("{name}.expect.json"));
    assert!(
        !bin.exists() && !expect.exists(),
        "{name}: the sync4 fixtures are written once: never rewrite them"
    );
    let path = p.dir.path().join(name);
    std::fs::write(&path, &out).unwrap();
    let t = FetchTarget {
        purpose,
        view: view.map(str::to_string),
        ..target(root, URL)
    };
    let canon = p.dst.store().reader().canon_head().unwrap();
    let views = p.dst.store().reader().view_head("work").unwrap();
    let got = fetch(&mut p.dst, &[&path], &t).unwrap();
    let reader = p.dst.store().reader();
    let v = json!({
        "rdy": "0.53.0",
        "stream_format": report.header.stream_format,
        "purpose": purpose,
        "stream": report,
        "upload_min_rdy": report.header.features.min_rdy(),
        "fetch": {
            "records_taken": got.records_taken,
            "frames_written": got.frames_written,
            "tags_taken": got.tags_taken,
            "tags_kept": got.tags_kept,
            "min_rdy_before": got.min_rdy_before,
            "min_rdy_after": got.min_rdy_after,
            "canon_moved": reader.canon_head().unwrap() != canon,
            "view_moved": reader.view_head("work").unwrap() != views,
        },
        "receiver_features": p.dst.store().features(),
    });
    std::fs::write(&bin, &out).unwrap();
    std::fs::write(&expect, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
}

/// Writes the two format-4 fixtures once, from a seeded world on the fixture clock. Run by hand,
/// never by the gate: `cargo test -p rdy-engine --test sync_fetch
/// scc2_write_sync4_fixtures_once -- --ignored`. It refuses a fixture that exists.
#[test]
#[ignore]
fn scc2_write_sync4_fixtures_once() {
    rdy_model::seed_ids_for_tests(2);
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [_, _, l3] = p.ls;
    write_fixture(
        &mut p,
        "sync4-canon-history",
        StreamPurpose::CanonHistory,
        l3,
        None,
    );
    let head = p
        .src
        .read()
        .reader()
        .view_head("work")
        .unwrap()
        .unwrap()
        .head;
    write_fixture(
        &mut p,
        "sync4-view-history",
        StreamPurpose::ViewHistory,
        head,
        Some("work"),
    );
}
