use rdy_model::*;
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
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
fn second_open_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let _s = Store::create(dir.path()).unwrap();
    assert!(matches!(
        Store::create(dir.path()),
        Err(StoreError::Locked(_))
    ));
}

#[test]
fn objects_persist_and_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let b = block("b-AAAAA", "x");
    let h = {
        let mut s = Store::create(dir.path()).unwrap();
        let h1 = s.put(kinds::BLOCK, &b).unwrap();
        let h2 = s.put(kinds::BLOCK, &b).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1, b.version_hash());
        h1
    };
    let s = Store::create(dir.path()).unwrap();
    assert_eq!(s.get_block(&h).unwrap(), b);
    assert!(matches!(
        s.get::<Block>(&Hash::of(b"missing")),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn blobs_chunk_and_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let data: Vec<u8> = (0..(5 * 1024 * 1024 / 2))
        .map(|i| (i % 251) as u8)
        .collect();
    let h = s.put_blob(&data).unwrap();
    assert_eq!(s.get_blob(&h).unwrap(), data);
    assert_eq!(s.put_blob(&data).unwrap(), h);
}

#[test]
fn commit_change_computes_deps_from_producers() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let v1 = block("b-AAAAA", "one");
    let c1 = s
        .commit_change(change(vec![Op::Create { block: v1.clone() }]), vec![])
        .unwrap();
    let v2 = block("b-AAAAA", "two");
    let c2 = s
        .commit_change(
            change(vec![Op::Update {
                base: v1.version_hash(),
                block: v2.clone(),
            }]),
            vec![],
        )
        .unwrap();
    let stored: Change = s.get(&c2).unwrap();
    assert_eq!(stored.deps, vec![c1]);
    assert_eq!(s.get_block(&v2.version_hash()).unwrap(), v2);
    let bad = change(vec![Op::Update {
        base: Hash::of(b"nope"),
        block: block("b-AAAAA", "x"),
    }]);
    assert!(matches!(
        s.commit_change(bad, vec![]),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn reindex_rebuilds_everything_including_refs() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = block("b-AAAAA", "one");
    {
        let mut s = Store::create(dir.path()).unwrap();
        s.commit_change(
            change(vec![Op::Create { block: v1.clone() }]),
            vec![("view/work".into(), vec![1])],
        )
        .unwrap();
        s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
        s.set_refs(vec![("canon".into(), b"B".to_vec())]).unwrap();
        s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap(); // same value again
    }
    std::fs::remove_file(dir.path().join("index.redb")).unwrap();
    let s = Store::create(dir.path()).unwrap();
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
    assert_eq!(s.get_ref("view/work").unwrap(), Some(vec![1]));
    assert_eq!(s.get_block(&v1.version_hash()).unwrap(), v1);
    let mut s = s;
    s.reindex().unwrap();
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
}

fn seg0(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("segments/seg-000000.log")
}

fn file_len(p: &std::path::Path) -> u64 {
    std::fs::metadata(p).unwrap().len()
}

/// A well-formed log frame: length, kind, payload hash, payload.
fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(&Hash::of(payload).0);
    out.extend_from_slice(payload);
    out
}

#[test]
fn failed_index_apply_rolls_back_frames_so_reindex_cannot_resurrect_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
    let len_before = file_len(&seg0(dir.path()));
    let frames_before = s.verify().unwrap();

    let v1 = block("b-AAAAA", "never acknowledged");
    s.fail_next_index_apply();
    assert!(
        s.commit_change(
            change(vec![Op::Create { block: v1.clone() }]),
            vec![("canon".into(), b"B".to_vec())]
        )
        .is_err()
    );
    assert_eq!(file_len(&seg0(dir.path())), len_before);
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));

    // the store keeps working, and the next write lands where the failed one started
    s.set_refs(vec![("other".into(), b"x".to_vec())]).unwrap();
    drop(s);
    let mut s = Store::create(dir.path()).unwrap();
    s.reindex().unwrap();
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
    assert!(!s.has(&v1.version_hash()).unwrap());
    assert_eq!(s.verify().unwrap(), frames_before + 1);
}

#[test]
fn failed_rollback_poisons_the_store_until_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
    s.fail_next_index_apply();
    s.fail_next_rollback();
    assert!(s.set_refs(vec![("canon".into(), b"B".to_vec())]).is_err());
    let e = s
        .set_refs(vec![("other".into(), b"x".to_vec())])
        .unwrap_err();
    assert!(e.to_string().contains("reopen"), "{e}");
    drop(s);
    let mut s = Store::create(dir.path()).unwrap();
    s.set_refs(vec![("other".into(), b"x".to_vec())]).unwrap();
}

#[test]
fn damaged_index_is_set_aside_and_rebuilt_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = block("b-AAAAA", "one");
    {
        let mut s = Store::create(dir.path()).unwrap();
        s.put(kinds::BLOCK, &v1).unwrap();
        s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
    }
    for round in 0..2 {
        let idx = dir.path().join("index.redb");
        let mut bytes = std::fs::read(&idx).unwrap();
        bytes[..512].fill(0xAB);
        std::fs::write(&idx, bytes).unwrap();
        let s = Store::create(dir.path()).unwrap();
        assert_eq!(s.get_block(&v1.version_hash()).unwrap(), v1);
        assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
        let damaged = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("index.redb.damaged-")
            })
            .count();
        assert_eq!(damaged, round + 1);
    }
}

#[test]
fn torn_unacknowledged_batch_with_a_valid_later_frame_is_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = block("b-AAAAA", "acknowledged");
    {
        let mut s = Store::create(dir.path()).unwrap();
        s.put(kinds::BLOCK, &v1).unwrap();
    }
    let seg = seg0(dir.path());
    let acked = file_len(&seg);
    // one unacknowledged batch: its first frame torn to zeros, its second frame intact
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes.extend(vec![0u8; 37 + 10]);
    bytes.extend(frame(kinds::BLOCK, b"later-frame-of-the-same-batch"));
    std::fs::write(&seg, &bytes).unwrap();

    let s = Store::create(dir.path()).unwrap();
    assert_eq!(s.get_block(&v1.version_hash()).unwrap(), v1);
    assert_eq!(file_len(&seg), acked);
    assert!(!s.has(&Hash::of(b"later-frame-of-the-same-batch")).unwrap());
    let q = dir
        .path()
        .join(format!("segments/seg-000000.log.torn-{acked}"));
    assert_eq!(std::fs::read(q).unwrap(), &bytes[acked as usize..]);
}

#[test]
fn corruption_below_the_acknowledged_boundary_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut s = Store::create(dir.path()).unwrap();
        s.put(kinds::BLOCK, &block("b-AAAAA", "one")).unwrap();
        s.put(kinds::BLOCK, &block("b-BBBBB", "two")).unwrap();
    }
    let seg = seg0(dir.path());
    let mut bytes = std::fs::read(&seg).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // inside the last acknowledged frame
    std::fs::write(&seg, &bytes).unwrap();

    let e = Store::create(dir.path()).err().expect("open must refuse");
    let StoreError::Io(io) = &e else {
        panic!("{e}")
    };
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidData);
    let msg = e.to_string();
    assert!(
        msg.contains("seg-000000.log") && msg.contains("move the damaged segment aside"),
        "{msg}"
    );
    assert_eq!(std::fs::read(&seg).unwrap(), bytes);
}

#[test]
fn a_malformed_canon_ref_is_a_decode_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
    assert!(matches!(s.canon_head(), Err(StoreError::Decode(_))));
    // every reader of canon goes through canon_head
    assert!(matches!(s.view("work"), Err(StoreError::Decode(_))));
    assert!(matches!(s.landings(), Err(StoreError::Decode(_))));
}

#[test]
fn an_index_that_points_a_hash_at_another_object_is_an_error_not_wrong_data() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    // same id and body length: both frames sit at the same location in their logs
    let va = block("b-AAAAA", "one");
    let vb = block("b-AAAAA", "two");
    Store::create(a_dir.path())
        .unwrap()
        .put(kinds::BLOCK, &va)
        .unwrap();
    Store::create(b_dir.path())
        .unwrap()
        .put(kinds::BLOCK, &vb)
        .unwrap();
    std::fs::copy(
        b_dir.path().join("index.redb"),
        a_dir.path().join("index.redb"),
    )
    .unwrap();
    let s = Store::create(a_dir.path()).unwrap();
    assert!(
        matches!(s.get_block(&vb.version_hash()), Err(StoreError::Decode(_))),
        "must not return {:?}",
        va.body
    );
}

/// Flips one byte at ~400 evenly spaced offsets of index.redb. Every flip must be survived:
/// open succeeds and every object and ref reads back exactly (the index is derived from the log,
/// so a damaged one is rebuilt, never trusted, and never a panic).
#[test]
fn a_byte_flip_anywhere_in_the_index_is_recovered_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let blocks: Vec<Block> = (0..50)
        .map(|i| block(&format!("b-{i:05}"), &format!("body {i}")))
        .collect();
    {
        let mut s = Store::create(dir.path()).unwrap();
        for b in &blocks {
            s.put(kinds::BLOCK, b).unwrap();
        }
        s.set_refs(vec![("canon".into(), blocks[0].version_hash().0.to_vec())])
            .unwrap();
    }
    let idx = dir.path().join("index.redb");
    let pristine = std::fs::read(&idx).unwrap();
    let step = (pristine.len() / 400).max(1);
    let mut failures = vec![];
    for off in (0..pristine.len()).step_by(step) {
        let mut bytes = pristine.clone();
        bytes[off] ^= 0xFF;
        std::fs::write(&idx, &bytes).unwrap();
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
                let s = Store::create(dir.path()).map_err(|e| e.to_string())?;
                for b in &blocks {
                    if s.get_block(&b.version_hash()).map_err(|e| e.to_string())? != *b {
                        return Err(format!("{} read back different", b.id));
                    }
                }
                if s.canon_head().map_err(|e| e.to_string())? != Some(blocks[0].version_hash()) {
                    return Err("canon ref lost".into());
                }
                Ok(())
            }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => failures.push(format!("offset {off}: {e}")),
            Err(_) => failures.push(format!("offset {off}: panicked")),
        }
        // the next round starts from the undamaged index; set-aside copies just accumulate
        std::fs::write(&idx, &pristine).unwrap();
    }
    assert!(
        failures.is_empty(),
        "{} flips were not recovered:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_partial_append_leaves_no_bytes_past_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
    let len_before = file_len(&seg0(dir.path()));
    let frames_before = s.verify().unwrap();
    let torn = block("b-AAAAA", "torn");
    s.fail_next_append_after(10);
    assert!(s.put(kinds::BLOCK, &torn).is_err());
    assert_eq!(
        file_len(&seg0(dir.path())),
        len_before,
        "the partial frame's bytes are discarded"
    );
    assert_eq!(
        s.verify().unwrap(),
        frames_before,
        "fsck in the same session sees only the acknowledged frames"
    );
    s.put(kinds::BLOCK, &block("b-BBBBB", "after")).unwrap();
    drop(s);
    let s = Store::create(dir.path()).unwrap();
    assert_eq!(s.verify().unwrap(), frames_before + 1);
    assert!(!s.has(&torn.version_hash()).unwrap());
}
