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

// ---- fix round 1: fetched histories stay valid ----

/// The head of view `name` in `r`.
fn view_head(r: &Rdy, name: &str) -> Hash {
    r.store().reader().view_head(name).unwrap().unwrap().head
}

/// The source's history of view `view` up to op `head`, as one batch file, with its fetch target.
fn view_batch(p: &Pair, view: &str, head: Hash, name: &str) -> (PathBuf, FetchTarget) {
    let mut out = Vec::new();
    stream_export::write_history(
        &p.src.read(),
        StreamPurpose::ViewHistory,
        head,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .unwrap();
    let path = p.dir.path().join(name);
    std::fs::write(&path, out).unwrap();
    let t = FetchTarget {
        purpose: StreamPurpose::ViewHistory,
        view: Some(view.into()),
        ..target(head, URL)
    };
    (path, t)
}

/// A project fork's origin batch in `r`: it clears every `meta/sync/` ref (D-3RSCW).
fn fork_origin(r: &mut Rdy) {
    let origin = rdy_store::meta::Origin {
        fork: true,
        repo_id: repo(r),
        tag: None,
        landing: r.store().reader().canon_head().unwrap(),
    };
    r.store_mut()
        .put_origin(&origin, Some(RepoId::new()))
        .unwrap();
    assert!(r.store().reader().sync_remotes().unwrap().is_empty());
}

/// A tag written straight to the store (no `rdy tag` rule), as a damaged or crafted ref is.
fn raw_tag(r: &mut Rdy, name: &str, landing: Hash) {
    r.store_mut()
        .put_tag(&Tag {
            name: name.into(),
            landing,
            message: None,
            tagged_by: Provenance {
                actor: "human:founder".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
        })
        .unwrap();
    r.seal_open().unwrap();
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

/// The text of `rdy fsck`'s refusal.
fn fsck_refusal(r: &Rdy) -> String {
    let e = r.fsck().expect_err("fsck refuses");
    assert_eq!(e.code, "E_IO", "{e:?}");
    e.message
}

#[test]
fn scc2_fetched_tags_pass_fsck_and_keep_their_checks() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, l2, l3] = p.ls;
    // a tagged fetch: the tag names a landing of the fetched history, canon stays at l1
    let file = canon_batch(&p, l3, Some(l1), "c");
    fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), Some(l1));
    assert_eq!(p.dst.fsck().unwrap().versions, 1);
    // a fetch of an earlier tip replaces the cursor: the tag stays a fetched history's
    let early = canon_batch(&p, l2, Some(l1), "c2");
    fetch(&mut p.dst, &[&early], &target(l2, URL)).unwrap();
    assert_eq!(
        p.dst
            .store()
            .reader()
            .sync_canon("origin")
            .unwrap()
            .unwrap()
            .fetched,
        Some(l2)
    );
    assert_eq!(p.dst.fsck().unwrap().versions, 1);
    // a project fork's origin batch clears every sync ref: the tag stays a fetched history's
    fork_origin(&mut p.dst);
    assert_eq!(p.dst.fsck().unwrap().versions, 1);
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), Some(l1));

    // a tag on a landing that only a view fetch brought: no canon fetch named its history
    let mut q = pair();
    let [_, _, q3] = q.ls;
    let head = view_head(&q.src, "work");
    let (vfile, vt) = view_batch(&q, "work", head, "v");
    fetch(&mut q.dst, &[&vfile], &vt).unwrap();
    assert!(
        has(&q.dst, kinds::LANDING, q3),
        "the view history holds the landing"
    );
    q.dst.fsck().unwrap();
    raw_tag(&mut q.dst, "v5.0.0", q3);
    let m = fsck_refusal(&q.dst);
    assert!(
        m.contains(&format!(
            "tag/v5.0.0: landing {} is not in canon's chain",
            q3.short()
        )),
        "{m}"
    );

    // the fetched tags keep their order on the fetched history: a higher version on an
    // earlier landing is refused
    let mut s = pair();
    let [s1, s2, s3] = s.ls;
    raw_tag(&mut s.src, "v3.0.0", s2);
    let file = canon_batch(&s, s3, Some(s1), "c");
    let got = fetch(&mut s.dst, &[&file], &target(s3, URL)).unwrap();
    assert_eq!(sorted(got.tags_taken), ["v2.0.0", "v3.0.0"]);
    let m = fsck_refusal(&s.dst);
    assert!(
        m.contains("tag/v3.0.0 names an earlier landing than tag/v2.0.0"),
        "{m}"
    );
}

#[test]
fn scc2_fetched_fork_closure_ops_pass_fsck() {
    let _clock = pin_clock(1_000_000);
    // the source extends work and forks it as branch; the receiver fetches branch, whose
    // closure holds work's new ops through the fork's source op
    let mut p = pair();
    let w = write(
        &mut p.src,
        "work",
        json!([{"op": "create", "type": "doc", "id": "$e", "fields": {"name": "b", "title": "B", "doc_type": "page"}}]),
    );
    let d = w.created["$e"].clone();
    let before_fork = view_head(&p.src, "work");
    p.src
        .view_fork(&session(Role::Writer), "work", "branch")
        .unwrap();
    write(
        &mut p.src,
        "branch",
        json!([{"op": "update", "id": d, "fields": {"title": "B2"}}]),
    );
    let branch = view_head(&p.src, "branch");
    let (file, t) = view_batch(&p, "branch", branch, "vb");
    fetch(&mut p.dst, &[&file], &t).unwrap();
    assert!(has(&p.dst, kinds::VIEW_OP, before_fork));
    assert!(
        p.dst
            .store()
            .reader()
            .sync_view("origin", "work")
            .unwrap()
            .is_none()
    );
    p.dst.store().check_objects().unwrap();
    p.dst.fsck().unwrap();
}

#[test]
fn scc2_fetched_view_ops_pass_fsck_after_a_project_fork() {
    let _clock = pin_clock(1_000_000);
    // a view fetch, then a project fork's origin batch, which clears every sync ref
    let mut p = pair();
    let head = view_head(&p.src, "work");
    let (file, t) = view_batch(&p, "work", head, "w");
    fetch(&mut p.dst, &[&file], &t).unwrap();
    p.dst.store().check_objects().unwrap();
    fork_origin(&mut p.dst);
    p.dst.store().check_objects().unwrap();
    p.dst.fsck().unwrap();
}

#[test]
fn scc2_fetched_view_ops_pass_fsck_after_the_cursor_moves_back() {
    let _clock = pin_clock(1_000_000);
    // a fetch of a newer head, then of an earlier one: the cursor moves back, the newer ops
    // stay held
    let mut p = pair();
    let earlier = view_head(&p.src, "work");
    write(
        &mut p.src,
        "work",
        json!([{"op": "create", "type": "doc", "id": "$f", "fields": {"name": "c", "title": "C", "doc_type": "page"}}]),
    );
    let newer = view_head(&p.src, "work");
    assert_ne!(earlier, newer);
    let (file, t) = view_batch(&p, "work", newer, "n");
    fetch(&mut p.dst, &[&file], &t).unwrap();
    let (file, t) = view_batch(&p, "work", earlier, "e");
    fetch(&mut p.dst, &[&file], &t).unwrap();
    assert_eq!(
        p.dst
            .store()
            .reader()
            .sync_view("origin", "work")
            .unwrap()
            .unwrap()
            .fetched,
        Some(earlier)
    );
    p.dst.store().check_objects().unwrap();
    p.dst.fsck().unwrap();
}

/// A landing `r` above `l2` in the source that names `l3` as its provenance (`rebased_from`),
/// as a replay's landing does; canon stays at `l3`. Returns its hash.
fn rebased_landing(p: &mut Pair) -> Hash {
    let [_, l2, l3] = p.ls;
    let rec = p.src.store().landing(&l3).unwrap();
    let r = p
        .src
        .store_mut()
        .put_landing_for_tests(&LandingRecord {
            prev: Some(l2),
            rebased_from: vec![l3],
            read_identity: None,
            ..rec
        })
        .unwrap();
    p.src.seal_open().unwrap();
    r
}

#[test]
fn scc2_fetched_tagged_rebased_from_landing_passes_fsck() {
    let _clock = pin_clock(1_000_000);
    // the fetched tip's chain is r, l2, l1: the tagged l3 joins its closure only as r's
    // provenance, with its tag v2.0.0
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    let r = rebased_landing(&mut p);
    assert_eq!(p.src.fsck().unwrap().versions, 1);
    let file = canon_batch(&p, r, Some(l1), "c");
    let got = fetch(&mut p.dst, &[&file], &target(r, URL)).unwrap();
    assert_eq!(got.tags_taken, ["v2.0.0"]);
    assert!(has(&p.dst, kinds::LANDING, l3));
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), Some(l1));
    p.dst.store().check_objects().unwrap();
    assert_eq!(p.dst.fsck().unwrap().versions, 1);
    // a project fork's origin batch clears every sync ref: the tag stays a fetched closure's
    fork_origin(&mut p.dst);
    assert_eq!(p.dst.fsck().unwrap().versions, 1);
    assert_eq!(p.dst.store().reader().canon_head().unwrap(), Some(l1));

    // the order holds on the provenance chain l3, l2, l1: a higher version on l2 is refused,
    // although the fetched tip's own chain holds only that one tag
    let mut s = pair();
    let [s1, s2, _] = s.ls;
    let r = rebased_landing(&mut s);
    raw_tag(&mut s.src, "v3.0.0", s2);
    let file = canon_batch(&s, r, Some(s1), "c");
    let got = fetch(&mut s.dst, &[&file], &target(r, URL)).unwrap();
    assert_eq!(sorted(got.tags_taken), ["v2.0.0", "v3.0.0"]);
    let m = fsck_refusal(&s.dst);
    assert!(
        m.contains("tag/v3.0.0 names an earlier landing than tag/v2.0.0"),
        "{m}"
    );
    assert!(!m.contains("not in canon's chain"), "{m}");
}

// ---- the relay-tag signature across a fetch ----

fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x61; 32])
}

fn key_data() -> ssh_key::public::KeyData {
    ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey(
        signing_key().verifying_key().to_bytes(),
    ))
}

fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    use ed25519_dalek::Signer as _;
    use ssh_encoding::Encode as _;
    let data = ssh_key::SshSig::signed_data(ns, ssh_key::HashAlg::Sha512, msg).unwrap();
    let sig = ssh_key::Signature::new(
        ssh_key::Algorithm::Ed25519,
        signing_key().sign(&data).to_bytes().to_vec(),
    )
    .unwrap();
    let mut out = vec![];
    ssh_key::SshSig::new(key_data(), ns, ssh_key::HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

/// Tags canon's head `name` in `r` through a relay (an agent for the founder) and signs its
/// relay statement in `r`'s repo. Returns the tag and its relay answer.
fn relay_tag(r: &mut Rdy, name: &str) -> (Hash, Hash) {
    use rdy_model::signature::{Signature, StatementSource, statement};
    use ssh_encoding::Encode as _;
    let agent = Session::agent("agent:a".into(), Some("run-1".into()), Role::Writer).unwrap();
    let relay = rdy_engine::answer::relay::RelayInput {
        evidence: Some("Yes, tag it.".into()),
        ..Default::default()
    };
    let info = r
        .tag_as(&agent, "human:founder", &relay, true, name, None, None)
        .unwrap();
    assert!(info.relayed_by.is_some());
    let t = tag_hash(r, name);
    let record = r.store().read_tag(&t).unwrap();
    let (answer_hash, answer) = r
        .store()
        .answer_of(&format!("tag:{name}"))
        .unwrap()
        .unwrap();
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    let stmt = statement(
        StatementSource::RelayTag {
            hash: t,
            tag: &record,
            answer_hash,
            answer: &answer,
        },
        Some(repo(r)),
        &rdy_engine::signing_crypto::blob_fingerprint(&blob),
    )
    .unwrap();
    let ns = rdy_model::signature::parse_statement(&stmt)
        .unwrap()
        .namespace()
        .to_string();
    let g = Signature {
        subject: t,
        namespace: ns.clone(),
        sig: sshsig(&ns, &stmt),
        statement: stmt,
        signed_by: Provenance {
            actor: "agent:s".into(),
            run: Some("s/1".into()),
            role: Role::Writer,
            at_ms: 1_000,
        },
    };
    r.store_mut().put_signature(&g).unwrap();
    r.seal_open().unwrap();
    (t, answer_hash)
}

/// The signature checks of subject `t` in `r`, as JSON.
fn sig_checks(r: &Rdy, t: Hash) -> Value {
    serde_json::to_value(
        rdy_engine::signature_check::signature_checks(&r.read(), &t, None).unwrap(),
    )
    .unwrap()
}

#[test]
fn scc2_fetched_relay_tag_keeps_its_answer_and_signature() {
    let _clock = pin_clock(1_000_000);
    let mut p = pair();
    let [l1, _, l3] = p.ls;
    let (t, answer) = relay_tag(&mut p.src, "v3.0.0");
    let source = sig_checks(&p.src, t);
    assert_eq!(
        (&source[0]["signed"], &source[0]["statement"]),
        (&json!("valid"), &json!("match")),
        "{source}"
    );
    // the fetch takes the free name with its relay answer, in its one batch
    let writes = p.dst.store().write_count();
    let file = canon_batch(&p, l3, Some(l1), "c");
    let got = fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert_eq!(sorted(got.tags_taken), ["v2.0.0", "v3.0.0"]);
    assert_eq!(p.dst.store().write_count(), writes + 1, "one batch");
    let reader = p.dst.store().reader();
    assert_eq!(reader.get_ref("tag/v3.0.0").unwrap(), Some(t.0.to_vec()));
    assert_eq!(
        reader.get_ref("answer/tag:v3.0.0").unwrap(),
        Some(answer.0.to_vec())
    );
    drop(reader);
    assert_eq!(sig_checks(&p.dst, t), source);
    p.dst.fsck().unwrap();
    // a retry writes nothing
    let again = fetch(&mut p.dst, &[&file], &target(l3, URL)).unwrap();
    assert!(again.unchanged);

    // a collision: the receiver's own relayed v3.0.0 keeps its tag and its answer
    let mut q = pair();
    let [q1, _, q3] = q.ls;
    let (theirs, _) = relay_tag(&mut q.src, "v3.0.0");
    let (local, local_answer) = relay_tag(&mut q.dst, "v3.0.0");
    let local_checks = sig_checks(&q.dst, local);
    let file = canon_batch(&q, q3, Some(q1), "c");
    let got = fetch(&mut q.dst, &[&file], &target(q3, URL)).unwrap();
    assert_eq!(got.tags_kept, ["v3.0.0"]);
    let reader = q.dst.store().reader();
    assert_eq!(
        reader.get_ref("tag/v3.0.0").unwrap(),
        Some(local.0.to_vec())
    );
    assert_eq!(
        reader.get_ref("answer/tag:v3.0.0").unwrap(),
        Some(local_answer.0.to_vec())
    );
    assert!(reader.has_kind(kinds::TAG, &theirs).unwrap());
    drop(reader);
    assert_eq!(sig_checks(&q.dst, local), local_checks);
    // the kept tag object, which no tag ref names, still reads its own relay answer
    let kept = sig_checks(&q.dst, theirs);
    assert_eq!(kept, sig_checks(&q.src, theirs));
    assert_eq!(
        (&kept[0]["signed"], &kept[0]["statement"]),
        (&json!("valid"), &json!("match")),
        "{kept}"
    );
    q.dst.fsck().unwrap();
}
