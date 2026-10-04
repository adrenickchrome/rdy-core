use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_store::{Store, StoreError, kinds, kinds::BlobManifest};
use serde_json::json;

#[test]
fn blob1_get_blob_refuses_objects_that_are_not_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let chunk = s.put(kinds::BLOB_CHUNK, &vec![1u8, 2, 3]).unwrap();
    let snap = s
        .put(kinds::SNAPSHOT, &rdy_model::StateSnapshot::default())
        .unwrap();
    for h in [chunk, snap] {
        assert!(
            matches!(s.get_blob(&h), Err(StoreError::Decode(_))),
            "{h:?}"
        );
        assert!(!s.has_blob(&h).unwrap());
    }
    let real = s.put_blob(b"hello").unwrap();
    assert!(s.has_blob(&real).unwrap());
    assert_eq!(s.get_blob(&real).unwrap(), b"hello");
}

#[test]
fn blob1_a_manifest_that_lies_about_its_length_is_an_error_not_an_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let chunk_bytes = postcard::to_allocvec(&vec![7u8; 10]).unwrap();
    let chunk = s.write(vec![(kinds::BLOB_CHUNK, chunk_bytes)]).unwrap()[0];
    for len in [u64::MAX, 0, 1] {
        let m = s
            .put(
                kinds::BLOB_MANIFEST,
                &BlobManifest {
                    len,
                    chunks: vec![chunk],
                },
            )
            .unwrap();
        assert!(
            matches!(s.get_blob(&m), Err(StoreError::Decode(_))),
            "len {len}"
        );
    }
    // a manifest listing another manifest as a chunk
    let inner = s.put_blob(b"inner").unwrap();
    let outer = s
        .put(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len: 5,
                chunks: vec![inner],
            },
        )
        .unwrap();
    assert!(matches!(s.get_blob(&outer), Err(StoreError::Decode(_))));
}

#[test]
fn blob1_a_figure_naming_a_non_blob_object_is_not_an_asset() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ])).unwrap();
    let doc = rdy_model::BlockId::parse(&res.created["$d"]).unwrap();
    let not_a_blob = r
        .block(&Scope::View("work".into()), &doc)
        .unwrap()
        .version_hash()
        .to_string();
    raw_commit(
        &mut r,
        "work",
        vec![raw(
            "fg-AAAAA",
            "figure",
            Some(&res.created["$s"]),
            "a1",
            &[
                ("blob", &not_a_blob),
                ("media_type", "image/png"),
                ("alt", "x"),
            ],
            "",
        )],
    );
    let site = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::View("work".into()), site.path()).unwrap();
    let page = std::fs::read_to_string(site.path().join("tx.html")).unwrap();
    assert!(page.contains("missing asset"), "{page}");
    let out = tempfile::tempdir().unwrap();
    let e = r
        .export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
}

/// A chunk dedupes to any stored object with the same bytes (the index is keyed by hash alone),
/// so the chunk's stored kind may be another kind's; its hash already proves its bytes.
#[test]
fn blob1_a_chunk_with_the_bytes_of_another_object_still_reads() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}}
    ])).unwrap();
    let doc = rdy_model::BlockId::parse(&res.created["$d"]).unwrap();
    let version = r
        .block(&Scope::View("work".into()), &doc)
        .unwrap()
        .version_hash();
    let (kind, payload) = r.store().get_raw(&version).unwrap();
    assert_eq!(kind, kinds::BLOCK);
    let blob = r.store_mut().put_blob(&payload).unwrap();
    assert!(r.store().has_blob(&blob).unwrap());
    assert_eq!(r.store().get_blob(&blob).unwrap(), payload);
    write(&mut r, "work", json!([{"op": "create", "type": "figure", "parent": res.created["$s"], "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "x"}}])).unwrap();
    let out = tempfile::tempdir().unwrap();
    r.export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap();
}

/// The reverse direction: a chunk uploaded first with the bytes a later block will have does
/// not make that block unreadable, and both still read.
#[test]
fn blob1_a_chunk_stored_first_does_not_make_a_later_block_unreadable() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}}
    ])).unwrap();
    let doc = rdy_model::BlockId::parse(&res.created["$d"]).unwrap();
    let block = r.block(&Scope::View("work".into()), &doc).unwrap();
    let payload = postcard::to_allocvec(&block).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let blob = s.put_blob(&payload).unwrap();
    let h = s.put(kinds::BLOCK, &block).unwrap();
    assert_eq!(h, block.version_hash());
    assert_eq!(s.get_raw(&h).unwrap().0, kinds::BLOB_CHUNK, "deduped");
    assert_eq!(s.get_block(&h).unwrap(), block);
    assert_eq!(s.get_blob(&blob).unwrap(), payload);
}

/// Truncates the last segment at every 97th byte (every 13th in deep mode) of its last write; every cut recovers
/// and the recovered store accepts a new write.
#[test]
fn store_torn_tail_at_sampled_offsets_recovers() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    // A torn write is one the index never acknowledged: keep the index as it was before the
    // last write, as after a crash between the segment append and the index commit.
    let (first, last, pre_index) = {
        let mut s = Store::open(&dir).unwrap();
        let first = s.put_blob(b"acknowledged").unwrap();
        let before = segment_len(&dir);
        let pre_index = std::fs::read(dir.join("index.redb")).unwrap();
        let last = s.put_blob(&vec![9u8; 4096]).unwrap();
        (first, (last, before), pre_index)
    };
    let (last_hash, cut_from) = last;
    let end = segment_len(&dir);
    let step = if deep() { 13 } else { 97 };
    for cut in (cut_from..end).step_by(step) {
        let copy = tempfile::tempdir().unwrap();
        copy_dir(&dir, copy.path());
        let seg = last_segment(copy.path());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&seg)
            .unwrap()
            .set_len(cut)
            .unwrap();
        if cut > cut_from {
            // with the index that acknowledged the write, a cut is damage, and must be refused
            assert!(
                Store::open(copy.path()).is_err(),
                "cut at {cut}: acknowledged data was cut silently"
            );
        }
        std::fs::write(copy.path().join("index.redb"), &pre_index).unwrap();
        let s = Store::open(copy.path()).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        assert_eq!(s.get_blob(&first).unwrap(), b"acknowledged", "cut at {cut}");
        assert!(
            s.get_blob(&last_hash).is_err(),
            "cut at {cut}: a torn write must not be visible"
        );
        s.verify().unwrap();
        // a torn tail left behind in the segment would corrupt this write or fail verify
        let mut s = s;
        let again = s.put_blob(b"after recovery").unwrap();
        assert_eq!(
            s.get_blob(&again).unwrap(),
            b"after recovery",
            "cut at {cut}"
        );
        s.verify().unwrap();
    }
}

#[test]
fn store_index_damage_sweep_never_returns_wrong_data() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let hashes: Vec<_> = {
        let mut s = Store::open(&dir).unwrap();
        (0..200u32)
            .map(|i| s.put_blob(&i.to_le_bytes()).unwrap())
            .collect()
    };
    let index = std::fs::read(dir.join("index.redb")).unwrap();
    let samples = if deep() { 400 } else { 40 };
    for k in 0..samples {
        let at = (index.len() * k) / samples;
        let copy = tempfile::tempdir().unwrap();
        copy_dir(&dir, copy.path());
        let mut bytes = index.clone();
        bytes[at] ^= 0xff;
        std::fs::write(copy.path().join("index.redb"), bytes).unwrap();
        let s = Store::open(copy.path()).unwrap_or_else(|e| panic!("flip at {at}: {e}"));
        for (i, h) in hashes.iter().enumerate() {
            assert_eq!(
                s.get_blob(h).unwrap(),
                (i as u32).to_le_bytes(),
                "flip at {at}"
            );
        }
    }
}

fn segments(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "log"))
        .collect();
    v.sort();
    v
}
fn last_segment(dir: &std::path::Path) -> std::path::PathBuf {
    segments(dir).pop().unwrap()
}
fn segment_len(dir: &std::path::Path) -> u64 {
    std::fs::metadata(last_segment(dir)).unwrap().len()
}
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &t)
        } else if p.file_name().unwrap() != "lock" {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}
