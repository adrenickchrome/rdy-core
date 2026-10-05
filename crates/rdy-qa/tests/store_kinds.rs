//! The (kind, hash) object index (plan M2a1, kind1): objects of different kinds with the same
//! bytes are separate objects, and a planted object never hides or impersonates another kind's.
use rdy_model::{Hash, StateSnapshot};
use rdy_qa::fixtures::*;
use rdy_store::{Store, StoreError, kinds, kinds::BlobManifest};
use serde_json::json;

const MIB: usize = 1024 * 1024;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("s")).unwrap();
    (dir, s)
}

fn plant(s: &mut Store, kind: u8, payload: Vec<u8>) -> Hash {
    s.write(vec![(kind, payload)]).unwrap()[0]
}

#[test]
fn kind1_one_hash_can_name_objects_of_several_kinds() {
    let (_d, mut s) = store();
    let bytes = b"same bytes, two kinds".to_vec();
    let a = plant(&mut s, kinds::BLOB_CHUNK, bytes.clone());
    let b = plant(&mut s, kinds::SNAPSHOT, bytes.clone());
    assert_eq!(a, b);
    assert!(s.has_kind(kinds::BLOB_CHUNK, &a).unwrap());
    assert!(s.has_kind(kinds::SNAPSHOT, &a).unwrap());
    assert!(!s.has_kind(kinds::TAG, &a).unwrap());
    assert_eq!(s.get_kind(kinds::SNAPSHOT, &a).unwrap(), bytes);
    assert_eq!(
        s.get_raw(&a).unwrap().0,
        kinds::BLOB_CHUNK,
        "untyped reads give the first written"
    );
    assert!(matches!(
        s.get_kind(kinds::TAG, &a),
        Err(StoreError::NotFound(_))
    ));
    let frames = s.verify().unwrap();
    plant(&mut s, kinds::SNAPSHOT, bytes);
    assert_eq!(
        s.verify().unwrap(),
        frames,
        "the same (kind, hash) still dedupes"
    );
}

#[test]
fn kind1_a_squatted_manifest_does_not_stand_in_for_the_blob() {
    let (_d, mut s) = store();
    let bytes = vec![5u8; 3 * MIB + 7];
    let manifest = postcard::to_allocvec(&BlobManifest {
        len: bytes.len() as u64,
        chunks: bytes.chunks(MIB).map(Hash::of).collect(),
    })
    .unwrap();
    let squat = plant(&mut s, kinds::BLOB_CHUNK, manifest);
    assert!(
        !s.has_blob(&squat).unwrap(),
        "a chunk-kind object is never a blob"
    );
    let h = s.put_blob(&bytes).unwrap();
    assert_eq!(h, squat);
    assert!(s.has_kind(kinds::BLOB_MANIFEST, &h).unwrap());
    assert!(s.has_blob(&h).unwrap());
    assert_eq!(s.get_blob(&h).unwrap(), bytes);
}

/// Review Focus 2: has_blob is an index lookup; it reads no frame, whatever the object is.
#[test]
fn kind1_has_blob_reads_no_frame() {
    let (_d, mut s) = store();
    let real = s.put_blob(&vec![1u8; 2 * MIB]).unwrap();
    let hc = plant(&mut s, kinds::BLOB_CHUNK, vec![9u8; MIB]);
    let forged = plant(
        &mut s,
        kinds::BLOB_CHUNK,
        postcard::to_allocvec(&BlobManifest {
            len: (64 * MIB) as u64,
            chunks: vec![hc; 64],
        })
        .unwrap(),
    );
    let before = s.frames_read();
    assert!(s.has_blob(&real).unwrap());
    assert!(!s.has_blob(&forged).unwrap());
    assert!(!s.has_blob(&hc).unwrap());
    assert_eq!(s.frames_read(), before);
}

#[test]
fn kind1_a_forged_manifest_naming_a_snapshot_is_not_a_blob() {
    let (_d, mut s) = store();
    let snap = StateSnapshot::default();
    let len = postcard::to_allocvec(&snap).unwrap().len() as u64;
    let sh = s.put(kinds::SNAPSHOT, &snap).unwrap();
    let forged = plant(
        &mut s,
        kinds::BLOB_CHUNK,
        postcard::to_allocvec(&BlobManifest {
            len,
            chunks: vec![sh],
        })
        .unwrap(),
    );
    assert!(!s.has_blob(&forged).unwrap());
    assert!(matches!(s.get_blob(&forged), Err(StoreError::Decode(_))));
}

#[test]
fn kind1_reindex_rebuilds_the_typed_entries() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::open(&dir).unwrap();
    let bytes = b"twice".to_vec();
    plant(&mut s, kinds::BLOB_CHUNK, bytes.clone());
    let h = plant(&mut s, kinds::SNAPSHOT, bytes);
    s.reindex().unwrap();
    assert!(s.has_kind(kinds::SNAPSHOT, &h).unwrap() && s.has_kind(kinds::BLOB_CHUNK, &h).unwrap());
    assert_eq!(s.get_raw(&h).unwrap().0, kinds::BLOB_CHUNK);
}

/// Guard (expected to pass before the change): a chunk is not a proposal.
#[test]
fn kind1_landing_a_hash_that_is_only_a_chunk_is_not_a_proposal() {
    let (_d, mut r) = project();
    write(&mut r, "work", json!([{"op": "create", "type": "doc", "fields": {"title": "One", "doc_type": "glossary"}}])).unwrap();
    let h = r
        .store_mut()
        .write(vec![(kinds::BLOB_CHUNK, b"not a proposal".to_vec())])
        .unwrap()[0];
    let e = r.land(&approver(), &h.to_string()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("is not a proposal"), "{e}");
}
