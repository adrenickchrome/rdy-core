//! mh4: `StoreHandle::compact` for the hub (rdyhub D-EYD92, core D-2G21C: `compact` takes `&self`).
//! It runs on the store's one writer, so an ordinary write waits for it; it appends no batch; a
//! holder compacts as a project does; a follower refuses.
use rdy_adapter::maintenance;
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{CompactReport, KindBytes, RdyError, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::{Hash, Role};
use rdy_store::compact::on_allocation;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// Lands a new doc `n` and tags it.
fn land_tag(r: &mut Rdy, n: usize) -> (String, Hash, Hash) {
    let name = format!("v{n}.0.0");
    let ops = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": "A", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("body {n}")}
    ]);
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), &name, None, None).unwrap();
    let (stored, hash, _) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .max_by_key(|(n, _, _)| n.clone())
        .unwrap();
    let landing = r.store().read_tag(&hash).unwrap().landing;
    (stored, hash, landing)
}

fn segment_files(root: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    out.sort();
    out
}

fn upload(r: &Rdy, tag: Hash, path: &Path) -> File {
    let mut out = File::create(path).unwrap();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    File::open(path).unwrap()
}

fn code(e: RdyError) -> String {
    e.code.to_string()
}

#[test]
fn mh4_adapter_compact_is_serialized_and_fallible() {
    let base = tempfile::tempdir().unwrap();
    let source = base.path().join("p");
    let mut r = Rdy::init(&source, "rdy-standard").unwrap();
    let (name1, tag1, landing1) = land_tag(&mut r, 1);

    // a project: the report is the engine's, and no batch is added
    let want = r.compact(false).unwrap();
    let dump = base.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    let upload1 = upload(&r, tag1, &base.path().join("u1.rdysync"));
    let (name2, tag2, landing2) = land_tag(&mut r, 2);
    let upload2 = upload(&r, tag2, &base.path().join("u2.rdysync"));
    let repo = r.store().repo_id().unwrap().unwrap();
    let again = r.compact(false).unwrap();
    assert!(again.log_bytes > want.log_bytes);
    r.close().unwrap();
    let handle = StoreHandle::open(&source).unwrap();
    let frames = handle.fsck().unwrap().frames;
    let bytes = segment_files(&source);
    let report: CompactReport = handle.compact().unwrap();
    assert_eq!(segment_files(&source), bytes, "no batch, no byte");
    assert_eq!(handle.fsck().unwrap().frames, frames);
    assert_eq!(report.log_bytes, again.log_bytes);
    assert_eq!(report.removed_bytes, again.removed_bytes);
    let rows: &Vec<KindBytes> = &report.by_kind;
    assert_eq!(rows, &again.by_kind);
    assert_eq!(report.notes, again.notes);
    handle.close().unwrap();

    // a holder compacts too, and an ordinary write through the same handle waits for it
    let holder_dir = base.path().join("h");
    let holder = StoreHandle::create_holder(&holder_dir, repo).unwrap();
    holder
        .take(
            std::slice::from_ref(&upload1),
            UploadTarget {
                tag: name1,
                landing: landing1,
            },
        )
        .unwrap();

    let (go, wait) = mpsc::channel::<()>();
    let (started, is_started) = mpsc::channel::<()>();
    let written = Arc::new(AtomicBool::new(false));
    let censuses = Arc::new(AtomicU32::new(0));
    let (written_in_hook, count) = (Arc::clone(&written), Arc::clone(&censuses));
    let held_off = Arc::new(AtomicBool::new(false));
    let held_off_in_hook = Arc::clone(&held_off);
    on_allocation(Some(Box::new(move |seg| {
        if seg == 0 && count.fetch_add(1, Ordering::SeqCst) == 0 {
            // inside the first census, on the store's writer: let the write start
            go.send(()).unwrap();
            is_started.recv().unwrap();
            for _ in 0..10_000 {
                std::thread::yield_now();
            }
        }
        held_off_in_hook.store(!written_in_hook.load(Ordering::SeqCst), Ordering::SeqCst);
        None
    })));
    let report = std::thread::scope(|scope| {
        let (holder, upload2) = (&holder, &upload2);
        let writer = scope.spawn(move || {
            wait.recv().unwrap();
            started.send(()).unwrap();
            let taken = holder.take(
                std::slice::from_ref(upload2),
                UploadTarget {
                    tag: name2,
                    landing: landing2,
                },
            );
            written.store(true, Ordering::SeqCst);
            taken
        });
        let report = holder.compact();
        // no compact call is left to race with the writer's end
        assert!(
            held_off.load(Ordering::SeqCst),
            "the write finished inside compact"
        );
        writer.join().unwrap().unwrap();
        report.unwrap()
    });
    on_allocation(None);
    assert_eq!(censuses.load(Ordering::SeqCst), 2);
    assert!(report.log_bytes > 0);
    assert!(report.notes.is_empty());
    // the write landed after compact's count, and compact itself added nothing
    let later = holder.compact().unwrap();
    assert!(later.log_bytes > report.log_bytes);
    holder.close().unwrap();

    // a follower never opens as a store handle, and its store refuses compact, dry run included
    let follower = base.path().join("f");
    maintenance::seed_follower(&dump, &follower).unwrap();
    assert_eq!(
        code(StoreHandle::open(&follower).err().unwrap()),
        "E_STORE_FOLLOWER"
    );
    let store = rdy_store::layout::open_follower_store(&follower.join(".rdy")).unwrap();
    assert!(store.log_bytes().is_ok(), "a follower's bytes can be read");
    for dry_run in [true, false] {
        let e: RdyError = store.compact(dry_run).unwrap_err().into();
        assert_eq!(code(e), "E_STORE_FOLLOWER");
    }
}
