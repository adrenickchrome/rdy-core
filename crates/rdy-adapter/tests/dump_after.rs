//! hc4 through the adapter (D-2FMRR, D-WDKWQ): a held store writes the increment after an earlier
//! dump through the lock its handle holds, from one captured state, with the engine's bytes. A
//! consumer builds every field of the summary from this crate's types alone.
use rdy_adapter::maintenance::{self, BatchMarker, ChainEnd, DumpKind, DumpSummary, Location};
use rdy_adapter::{Hash, RdyError, RepoId, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::kinds;
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

/// Lands one new prose block in `r` and tags the landing `name`.
fn land_tag(r: &mut Rdy, n: usize, name: &str) {
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
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

/// The objects of `src`'s log from `from` on that a holder keeps.
fn objects(src: &Rdy, from: Location) -> Vec<(u8, Vec<u8>)> {
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
        .for_each_frame_from(from, src.store().log_end(), |_, kind, _, payload, _| {
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

/// Writes `items` and `refs` into the closed holder `dir`, in one sealed batch.
fn grow(dir: &Path, items: Vec<(u8, Vec<u8>)>, refs: Vec<(String, Vec<u8>)>) {
    let mut h = Rdy::open_holder(dir).unwrap();
    h.store_mut().write(items).unwrap();
    h.store_mut().set_refs(refs).unwrap();
    h.seal_open().unwrap();
    h.close().unwrap();
}

fn expect_err<T>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[test]
fn hc4_adapter_matches_engine_and_never_reopens_holder() {
    let root = tempfile::tempdir().unwrap();
    let mut src = Rdy::init(&root.path().join("src"), "rdy-standard").unwrap();
    land_tag(&mut src, 0, "v1.0.0");
    let repo = RepoId([5; 16]);
    let dir: PathBuf = root.path().join("holder");
    {
        let mut h = Rdy::create_holder(&dir, repo).unwrap();
        h.store_mut().write(objects(&src, ORIGIN)).unwrap();
        h.store_mut().set_refs(tag_refs(&src, &["v1.0.0"])).unwrap();
        h.seal_open().unwrap();
        h.close().unwrap();
    }
    let h = StoreHandle::open_holder(&dir).unwrap();
    let full_path = root.path().join("full.rdydump");
    let full = h.dump(&full_path).unwrap();
    let full_bytes = bytes(&full_path);
    assert_eq!(full.kind, DumpKind::Full);
    // nothing after the full dump yet: no file
    let none = root.path().join("none.rdydump");
    let empty = h.dump_after(&full.chain_end(), &none).unwrap();
    assert_eq!(empty.out, None);
    assert_eq!(empty.chain_end(), full.chain_end());
    assert!(!none.exists());
    h.close().unwrap();

    // the holder takes later history
    let mark = src.store().log_end();
    land_tag(&mut src, 1, "v1.1.0");
    land_tag(&mut src, 2, "v1.2.0");
    grow(
        &dir,
        objects(&src, mark),
        tag_refs(&src, &["v1.1.0", "v1.2.0"]),
    );

    let h = StoreHandle::open_holder(&dir).unwrap();
    let a = root.path().join("a.rdydump");
    let inc = h.dump_after(&full.chain_end(), &a).unwrap();
    // through the handle's own lock: nobody else can open the holder, before or after
    assert_eq!(expect_err(StoreHandle::open_holder(&dir)).code, "E_LOCKED");
    assert_eq!(inc.kind, DumpKind::Increment);
    assert_eq!(inc.base, full.end);
    assert!(inc.end > full.end);
    assert!(inc.files.is_empty(), "a holder carries no project file");
    assert!(inc.batches >= 1);
    // a predecessor that does not fit is named `previous dump`
    let e = expect_err(h.dump_after(
        &ChainEnd {
            repo_id: RepoId([6; 16]),
            ..full.chain_end()
        },
        &root.path().join("bad.rdydump"),
    ));
    assert_eq!(e.code, "E_DUMP_CHAIN");
    assert_eq!(
        e.message,
        format!(
            "previous dump holds repo {}, but this project is repo {repo}",
            RepoId([6; 16])
        )
    );
    assert!(!root.path().join("bad.rdydump").exists());
    // a consumer builds the whole summary from this crate's types
    let built = DumpSummary {
        out: Some(a.display().to_string()),
        dump_format: 3,
        kind: DumpKind::Increment,
        store_format: inc.store_format,
        repo_id: repo,
        base: Location {
            segment: full.end.segment,
            offset: full.end.offset,
        },
        end: inc.end,
        end_batch: BatchMarker {
            start: inc.end_batch.start,
            frames: inc.end_batch.frames,
            digest: Hash(inc.end_batch.digest.0),
        },
        digest: inc.digest,
        removals: 0,
        first_removed: None,
        batches: inc.batches,
        frames: inc.frames,
        files: vec![],
        bytes: inc.bytes,
    };
    assert_eq!(built, inc);
    let facts = maintenance::inspect_dump(&a).unwrap();
    assert_eq!(facts.kind, Some(DumpKind::Increment));
    assert_eq!(facts.prev, Some(full.digest));
    h.close().unwrap();

    // the engine, on the same state, writes the same bytes and summary
    let engine = Rdy::open_holder(&dir).unwrap();
    let b = root.path().join("b.rdydump");
    let direct = engine.read().dump_after(&full.chain_end(), &b).unwrap();
    assert_eq!(bytes(&a), bytes(&b));
    assert_eq!(
        DumpSummary {
            out: inc.out.clone(),
            ..direct
        },
        inc
    );
    // the predecessor was never changed
    assert_eq!(bytes(&full_path), full_bytes);
    engine.close().unwrap();
}
