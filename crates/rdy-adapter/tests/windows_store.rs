//! wx12c: the adapter's store handles on every platform. A handle, a captured snapshot or a
//! finished load holds no file open beyond its life, so the directory can be moved and removed
//! (a Windows delete fails while any handle is open), and an embedded read gives the hashes of a
//! direct engine read.
use rdy_adapter::StoreHandle;
use rdy_adapter::maintenance;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use serde_json::json;
use std::path::Path;

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// A project with one landed page and the tag `v1.0.0`, closed.
fn project(root: &Path) {
    let mut r = Rdy::init(root, "rdy-standard").unwrap();
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d0", "title": "D0", "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "A page."}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "Work", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    r.close().unwrap();
}

/// Moves `dir` aside and removes it: both fail on Windows while a handle into it is open.
fn move_and_remove(dir: &Path) {
    let aside = dir.with_file_name(format!(
        "{}-aside",
        dir.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(dir, &aside).unwrap();
    std::fs::remove_dir_all(&aside).unwrap();
    assert!(!dir.exists() && !aside.exists());
}

#[test]
fn wx12c_adapter_load_releases_install_handles() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    project(&src);
    let dump = d.path().join("a.rdydump");
    let h = StoreHandle::open(&src).unwrap();
    h.dump(&dump).unwrap();
    // an open store is not opened twice
    let blocked = match StoreHandle::open(&src) {
        Ok(_) => panic!("a second open must be refused"),
        Err(e) => e,
    };
    assert_eq!(blocked.code, "E_LOCKED", "{blocked}");
    h.close().unwrap();

    // a finished load leaves nothing open in its target
    let loaded = d.path().join("loaded");
    maintenance::load(&dump, &loaded).unwrap();
    move_and_remove(&loaded);

    // a load, an open and a close in turn leave nothing open
    let again = d.path().join("again");
    maintenance::load(&dump, &again).unwrap();
    StoreHandle::open(&again).unwrap().close().unwrap();
    move_and_remove(&again);

    // a seeded follower is released too
    let follower = d.path().join("follower");
    maintenance::seed_follower(&dump, &follower).unwrap();
    move_and_remove(&follower);
}

#[test]
fn wx12c_direct_and_embedded_reads_keep_identical_hashes() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    project(&root);

    // the direct engine read
    let direct = Rdy::open(&root).unwrap();
    let direct_canon = direct.read().canon_head().unwrap();
    let direct_tags: Vec<_> = direct
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(n, h, _)| (n, h))
        .collect();
    let direct_dump = d.path().join("direct.rdydump");
    direct.read().dump(&direct_dump).unwrap();
    direct.close().unwrap();

    // the embedded read, and a snapshot that outlives its handle
    let h = StoreHandle::open(&root).unwrap();
    let snap = h.snapshot().unwrap();
    let embedded_dump = d.path().join("embedded.rdydump");
    h.dump(&embedded_dump).unwrap();
    h.close().unwrap();
    assert_eq!(snap.canon(), Some(direct_canon));
    let tags = snap.tags().unwrap();
    assert_eq!(tags.len(), direct_tags.len());
    for ((hash, tag), (name, direct_hash)) in tags.iter().zip(&direct_tags) {
        assert_eq!((&tag.name, hash), (name, direct_hash));
    }
    assert_eq!(
        std::fs::read(&direct_dump).unwrap(),
        std::fs::read(&embedded_dump).unwrap()
    );

    // a held snapshot blocks an unsafe reopen; once it is released the store opens again
    let blocked = match StoreHandle::open(&root) {
        Ok(_) => panic!("a reopen under a held snapshot must be refused"),
        Err(e) => e,
    };
    assert_eq!(blocked.code, "E_HELD_BY_READER", "{blocked}");
    drop(snap);
    StoreHandle::open(&root).unwrap().close().unwrap();
    move_and_remove(&root);
}
