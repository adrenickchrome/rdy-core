//! rdr1: a cloneable reader half that reads what the writer acknowledged and nothing else.
use rdy_model::{Block, BlockId, ChangeLinks, Hash, Op, Provenance, Role};
use rdy_store::{Store, StoreReader, kinds};
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};

fn assert_send_sync<T: Send + Sync>() {}

fn block(id: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn change(view: &str, ops: Vec<Op>) -> rdy_model::Change {
    rdy_model::Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "someone".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
    }
}

#[test]
fn rdr1_the_reader_is_send_sync_and_clone() {
    assert_send_sync::<StoreReader>();
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let r = s.reader();
    let _r2 = r.clone();
}

#[test]
fn rdr1_a_reader_reads_through_a_roll_and_never_past_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.set_segment_max(2_000);
    let r = s.reader();
    let start = s.log_end();
    let mut hashes = vec![];
    for i in 0..40u32 {
        let payload = vec![i as u8; 300];
        hashes.push(s.write(vec![(kinds::BLOB_CHUNK, payload)]).unwrap()[0]);
    }
    assert!(s.log_end().segment >= 2, "the small max rolled segments");
    for (i, h) in hashes.iter().enumerate() {
        let (k, p) = r.get_raw(h).unwrap();
        assert_eq!((k, p.len(), p[0]), (kinds::BLOB_CHUNK, 300, i as u8));
    }
    assert_eq!(r.frames_read(), 40, "one frame per read, shared counter");
    let mut seen = 0;
    r.for_each_frame_from(start, s.log_end(), |_, k, _, _, _| {
        if k != kinds::BATCH_COMMIT {
            seen += 1;
        }
        Ok::<(), rdy_store::StoreError>(())
    })
    .unwrap();
    assert_eq!(seen, 40, "every data frame once; markers skipped by kind");
}

#[test]
fn rdr1_last_batch_names_the_frames_of_the_last_write_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    assert!(s.last_batch().is_empty());
    let a = s
        .write(vec![
            (kinds::BLOB_CHUNK, b"a".to_vec()),
            (kinds::BLOB_CHUNK, b"b".to_vec()),
        ])
        .unwrap();
    assert_eq!(
        s.last_batch(),
        &[(kinds::BLOB_CHUNK, a[0]), (kinds::BLOB_CHUNK, a[1])]
    );
    assert_eq!(s.last_batch_end(), s.log_end());
    // a fully deduped write records an empty batch at the unchanged end
    let end = s.log_end();
    s.write(vec![(kinds::BLOB_CHUNK, b"a".to_vec())]).unwrap();
    assert!(s.last_batch().is_empty());
    assert_eq!(s.last_batch_end(), end);
    // a failed write leaves the previous record
    s.write(vec![(kinds::BLOB_CHUNK, b"c".to_vec())]).unwrap();
    let kept = s.last_batch().to_vec();
    s.fail_next_index_apply();
    assert!(s.write(vec![(kinds::BLOB_CHUNK, b"d".to_vec())]).is_err());
    assert_eq!(s.last_batch(), kept.as_slice());
}

#[test]
fn rdr1_commit_to_view_with_checks_bases_through_the_closure() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let b = block("b-AAAAA", "claim", "x");
    let v1 = b.version_hash();
    let create = change("v", vec![Op::Create { block: b.clone() }]);
    s.commit_to_view_with("v", create, &|_| None).unwrap();
    let mut b2 = b.clone();
    b2.body = "y".into();
    let update = change(
        "v",
        vec![Op::Update {
            base: v1,
            block: b2,
        }],
    );
    let stale = s.commit_to_view_with("v", update.clone(), &|_| Some(Hash([9; 32])));
    assert!(
        matches!(stale, Err(rdy_store::StoreError::StaleBase { .. })),
        "{stale:?}"
    );
    s.commit_to_view_with("v", update, &|id: &BlockId| (id == &b.id).then_some(v1))
        .unwrap();
    assert_eq!(s.included("v").unwrap().len(), 2);
}

#[test]
fn rdr1_a_reader_survives_a_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let h = s
        .write(vec![(kinds::BLOB_CHUNK, b"keep".to_vec())])
        .unwrap()[0];
    let old = s.reader();
    s.reindex().unwrap();
    assert_eq!(
        old.get_raw(&h).unwrap().1,
        b"keep",
        "the old handle still reads its index"
    );
    assert_eq!(
        s.reader().get_raw(&h).unwrap().1,
        b"keep",
        "the new handle reads the rebuilt one"
    );
    let h2 = s
        .write(vec![(kinds::BLOB_CHUNK, b"after".to_vec())])
        .unwrap()[0];
    assert_eq!(s.reader().get_raw(&h2).unwrap().1, b"after");
}

#[test]
fn rdr1_concurrent_readers_see_only_acknowledged_frames() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(std::sync::Mutex::new(Store::open(dir.path()).unwrap()));
    let r = s.lock().unwrap().reader();
    let start = Arc::new(Barrier::new(3));
    let writer = {
        let (s, start) = (Arc::clone(&s), Arc::clone(&start));
        std::thread::spawn(move || {
            start.wait();
            let mut out = vec![];
            for i in 0..200u32 {
                out.push(
                    s.lock()
                        .unwrap()
                        .write(vec![(kinds::BLOB_CHUNK, i.to_le_bytes().to_vec())])
                        .unwrap()[0],
                );
            }
            out
        })
    };
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (r, start) = (r.clone(), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                let mut ok = 0;
                for i in 0..200u32 {
                    let h = Hash::of(&i.to_le_bytes());
                    match r.get_raw(&h) {
                        Ok((_, p)) => {
                            assert_eq!(p, i.to_le_bytes());
                            ok += 1;
                        }
                        Err(rdy_store::StoreError::NotFound(_)) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
                ok
            })
        })
        .collect();
    let hashes = writer.join().unwrap();
    for t in readers {
        t.join().unwrap();
    }
    for h in hashes {
        assert!(r.get_raw(&h).is_ok());
    }
}

#[test]
fn rdr1_a_reader_that_outlives_its_store_makes_a_reopen_refuse_not_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let h = s.write(vec![(kinds::BLOB_CHUNK, b"x".to_vec())]).unwrap()[0];
    let r = s.reader();
    drop(s);
    assert!(
        matches!(
            Store::open(dir.path()),
            Err(rdy_store::StoreError::Locked(_))
        ),
        "a live reader still holds the index"
    );
    let set_aside = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains("damaged"));
    assert!(!set_aside, "the index was not set aside");
    assert_eq!(r.get_raw(&h).unwrap().1, b"x");
    drop(r);
    let s = Store::open(dir.path()).unwrap();
    assert!(!s.open_report().index_rebuilt);
}
