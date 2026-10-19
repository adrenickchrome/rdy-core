//! Holder maintenance through the adapter (D-WWAK7, D-69AY3, D-BPYN8): a holder is created,
//! opened, dumped, checked and loaded through the existing engine paths, with no project file and
//! no second open of a store the handle already holds.
use rdy_adapter::maintenance::{self, DumpPair};
use rdy_adapter::{RdyError, RepoId, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::json;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

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

/// Lands one change in `r` that sets the prose block `id` (or creates the block when `id` is
/// None), tags the landing `name`, and returns the block id.
fn land_tag(r: &mut Rdy, id: Option<&str>, body: &str, name: &str) -> String {
    let ops = match id {
        None => json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
        Some(id) => json!([{"op": "update", "id": id, "body": body}]),
    };
    let w = r
        .write(
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
        .propose(&session(Role::Writer), "work", "T", None)
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    r.tag(&session(Role::Approver), name, None, None).unwrap();
    id.map(str::to_string)
        .unwrap_or_else(|| w.created["$p"].clone())
}

fn objects(src: &Rdy) -> Vec<(u8, Vec<u8>)> {
    const COPIED: [u8; 8] = [
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::BLOB_CHUNK,
        kinds::BLOB_MANIFEST,
        kinds::TAG,
    ];
    let mut out = vec![];
    src.store()
        .for_each_frame_from(ORIGIN, src.store().log_end(), |_, kind, _, payload, _| {
            if COPIED.contains(&kind) {
                out.push((kind, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    out
}

fn tag_refs(src: &Rdy, names: &[&str]) -> Vec<(String, Vec<u8>)> {
    src.store()
        .tags()
        .unwrap()
        .into_iter()
        .filter(|(n, _, _)| names.contains(&n.as_str()))
        .map(|(n, h, _)| (format!("tag/{n}"), h.0.to_vec()))
        .collect()
}

/// A closed holder at `<root>/holder` with two divergent histories and three tags.
fn divergent_holder(root: &Path, repo_id: RepoId) -> PathBuf {
    let mut left = Rdy::init(&root.join("left"), "rdy-standard").unwrap();
    let prose = land_tag(&mut left, None, "base", "v1.0.0");
    let dump = root.join("left.rdydump");
    left.read().dump(&dump).unwrap();
    maintenance::load(&dump, &root.join("right")).unwrap();
    let mut right = Rdy::open(&root.join("right")).unwrap();
    land_tag(&mut left, Some(&prose), "left", "v1.1.0");
    land_tag(&mut right, Some(&prose), "right", "v2.0.0");
    let dir = root.join("holder");
    let mut items = objects(&left);
    items.extend(objects(&right));
    let mut refs = tag_refs(&left, &["v1.0.0", "v1.1.0"]);
    refs.extend(tag_refs(&right, &["v2.0.0"]));
    let mut h = Rdy::create_holder(&dir, repo_id).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
    dir
}

fn fsck_json(h: &StoreHandle) -> serde_json::Value {
    serde_json::to_value(h.fsck().unwrap()).unwrap()
}

fn expect_err<T>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

#[test]
fn ha12_create_open_and_refuse_across_modes() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("h");
    let id = RepoId([2; 16]);
    let h = StoreHandle::create_holder(&dir, id).unwrap();
    // created holders hold the lock until closed
    assert_eq!(expect_err(StoreHandle::open_holder(&dir)).code, "E_LOCKED");
    assert_eq!(h.fsck().unwrap().versions, 0);
    h.close().unwrap();
    // nothing but the store was written
    assert!(!dir.join(".rdy.yaml").exists());
    assert!(!dir.join(".rdy/packs").exists());
    let h = StoreHandle::open_holder(&dir).unwrap();
    h.close().unwrap();
    // a holder is no project, and a project is no holder
    // (the project path reads its config first, so a holder with none is refused there)
    expect_err(StoreHandle::open(&dir));
    let project = root.path().join("p");
    Rdy::init(&project, "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    let e = expect_err(StoreHandle::open_holder(&project));
    assert_eq!(e.message, "this store is a project");
    // a second create over a store is refused
    assert!(StoreHandle::create_holder(&dir, id).is_err());
}

#[test]
fn ha12_dump_from_a_held_adapter_does_not_reopen_the_store() {
    let root = tempfile::tempdir().unwrap();
    let dir = divergent_holder(root.path(), RepoId([6; 16]));
    let h = StoreHandle::open_holder(&dir).unwrap();
    let before = fsck_json(&h);
    let out = root.path().join("held.rdydump");
    let summary = h.dump(&out).unwrap();
    assert!(summary.files.is_empty(), "{summary:?}");
    assert!(summary.batches > 0 && out.exists());
    // the lock is still ours: the dump did not release it, and nobody else took it
    assert_eq!(expect_err(StoreHandle::open_holder(&dir)).code, "E_LOCKED");
    // fsck is a read: it logs nothing, so a second dump has the same batches
    assert_eq!(fsck_json(&h), before);
    let again = h.dump(&root.path().join("held-2.rdydump")).unwrap();
    assert_eq!(again.batches, summary.batches);
    assert_eq!(again.frames, summary.frames);
    // the dump never overwrites a file
    assert_eq!(expect_err(h.dump(&out)).code, "E_ALREADY_EXISTS");
    h.close().unwrap();
}

#[test]
fn ha12_format_calls_need_no_store() {
    let root = tempfile::tempdir().unwrap();
    // nothing is open and no directory is a store: the format calls still answer
    let facts = maintenance::formats();
    let engine = rdy_engine::formats::formats();
    assert_eq!(facts, engine);
    assert!(facts.reads.contains(&DumpPair {
        dump: facts.dump,
        store: facts.store
    }));

    let dir = divergent_holder(root.path(), RepoId([8; 16]));
    let h = StoreHandle::open_holder(&dir).unwrap();
    let dump = root.path().join("h.rdydump");
    h.dump(&dump).unwrap();
    let source = fsck_json(&h);
    h.close().unwrap();

    let seen = maintenance::inspect_dump(&dump).unwrap();
    assert_eq!((seen.dump, seen.store), (facts.dump, facts.store));
    assert_eq!(seen.rdy, facts.rdy);
    let e = expect_err(maintenance::inspect_dump(&root.path().join("missing")));
    assert_eq!(e.code, "E_BAD_INPUT");

    // load through the adapter: a holder, byte for byte, with the holder's own next step
    let target = root.path().join("loaded");
    let loaded = maintenance::load(&dump, &target).unwrap();
    assert_eq!(loaded.next, "read the holder through the adapter at a tag");
    assert!(loaded.files_written.is_empty());
    assert_eq!(serde_json::to_value(&loaded.fsck).unwrap(), source);
    let got = StoreHandle::open_holder(&target).unwrap();
    assert_eq!(fsck_json(&got), source);
    got.close().unwrap();
    // a load over the same target is refused and leaves it alone
    let e = expect_err(maintenance::load(&dump, &target));
    assert_eq!(e.code, "E_ALREADY_EXISTS");
    StoreHandle::open_holder(&target).unwrap().close().unwrap();
}
