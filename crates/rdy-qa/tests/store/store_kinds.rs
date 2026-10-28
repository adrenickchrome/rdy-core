//! The (kind, hash) object index (plan M2a1, kind1): objects of different kinds with the same
//! bytes are separate objects, and a planted object never hides or impersonates another kind's.
use rdy_model::{
    ActorRun, AltChoice, ApprovalEvidence, Assurance, EvidenceKind, LandingRecord, RelayEvidence,
};
use rdy_model::{Hash, StateSnapshot};
use rdy_model::{Provenance, Role, Tag};
use rdy_qa::fixtures::*;
use rdy_store::{Store, StoreError, kinds, kinds::BlobManifest};
use serde_json::json;

const MIB: usize = 1024 * 1024;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create(&dir.path().join("s")).unwrap();
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
    let mut s = Store::create(&dir).unwrap();
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
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"title": "One", "doc_type": "page"}}]),
    )
    .unwrap();
    let h = r
        .store_mut()
        .write(vec![(kinds::BLOB_CHUNK, b"not a proposal".to_vec())])
        .unwrap()[0];
    let e = r.land(&approver(), &h.to_string()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("is not a proposal"), "{e}");
}

#[test]
fn kind1_a_chunk_planted_with_a_tags_bytes_does_not_hide_the_tag() {
    let (_d, mut s) = store();
    let tag = Tag {
        name: "v1.0.0".into(),
        landing: Hash::of(b"landing"),
        message: None,
        tagged_by: Provenance {
            actor: "human:qa".into(),
            run: None,
            role: Role::Approver,
            at_ms: 7,
        },
    };
    let planted = plant(
        &mut s,
        kinds::BLOB_CHUNK,
        postcard::to_allocvec(&tag).unwrap(),
    );
    let id = s.put_tag(&tag).unwrap();
    assert_eq!(id, planted);
    assert_eq!(s.read_tag(&id).unwrap(), tag);
    assert_eq!(s.tags().unwrap().len(), 1);
}

/// Gate listing for `repr1`: its other tests live in the product crates, which the gate runs
/// but does not list. A landing with every field set reads back in its kind, with its stored
/// hash as id, from the store and after a reindex.
#[test]
fn repr1_a_landing_reads_back_in_its_kind_with_its_id_after_a_reindex() {
    let (_d, mut s) = store();
    let by = Provenance {
        actor: "someone".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    };
    let l = LandingRecord {
        proposal: Hash::of(b"p"),
        prev: None,
        state: StateSnapshot::default().id(),
        approved_by: by,
        changes: vec![Hash::of(b"c")],
        chosen: Some(AltChoice {
            set: Hash::of(b"set"),
            member: "alt-a".into(),
        }),
        checks: vec![],
        assurance: Assurance::Relayed,
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("run-1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "land it".into(),
            session_ref: "sess-1".into(),
            question: Some("land it?".into()),
        }),
        approved_version: Some(Hash::of(b"p")),
        rebased_from: vec![Hash::of(b"an older landing")],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Text,
            options: vec!["land".into(), "wait".into()],
            choice: Some("land".into()),
            card_hash: None,
        }),
        assessment: None,
        policy: None,
        read_identity: None,
    };
    // the landing's frame at its latest encoding
    let (raw, payload) = rdy_store::codec::item(kinds::LANDING, &l).unwrap();
    let lh = plant(&mut s, raw, payload);
    assert_eq!(lh, l.id());
    assert!(s.has_kind(kinds::LANDING, &lh).unwrap());
    assert_eq!(s.landing(&lh).unwrap(), l);
    s.reindex().unwrap();
    assert_eq!(s.landing(&lh).unwrap(), l);
}
