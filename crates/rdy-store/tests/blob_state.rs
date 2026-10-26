//! A blob's state read from its metadata alone (D-FY4VF, D-P687F; n-62GWK): `blob_state` reads
//! the manifest and the redaction masks, never a chunk's bytes.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::{BlobState, Store, StoreError};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;

const MIB: usize = 1024 * 1024;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create(&dir.path().join("s")).unwrap();
    (dir, s)
}

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
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// The refusal text of a decode error.
fn decode_text<T: std::fmt::Debug>(r: Result<T, StoreError>) -> String {
    match r {
        Err(StoreError::Decode(m)) => m,
        other => panic!("not a decode error: {other:?}"),
    }
}

#[test]
fn dgm2_state_reads_no_chunk_bytes() {
    let (_d, mut s) = store();
    // a canonical one-chunk manifest whose chunk was never written: its bytes are unavailable
    let absent = Hash::of(b"a chunk no frame holds");
    let m = s
        .put(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len: 5,
                chunks: vec![absent],
            },
        )
        .unwrap();
    assert!(s.has_blob(&m).unwrap());
    assert_eq!(s.blob_state(&m).unwrap(), BlobState::Readable);
    // a byte read still reports the absent chunk
    assert!(
        matches!(s.get_blob(&m), Err(StoreError::NotFound(h)) if h == absent),
        "the byte read fails"
    );
    // and what put_blob writes is readable too, by metadata and by bytes
    let h = s.put_blob(b"a figure's bytes").unwrap();
    assert_eq!(s.blob_state(&h).unwrap(), BlobState::Readable);
    assert_eq!(s.get_blob(&h).unwrap(), b"a figure's bytes");
}

#[test]
fn dgm2_missing_manifest_is_missing() {
    let (_d, mut s) = store();
    // no object at all
    assert_eq!(
        s.blob_state(&Hash::of(b"nothing")).unwrap(),
        BlobState::Missing
    );
    // an object of another kind: a chunk, and a block
    let chunk = s
        .write(vec![(kinds::BLOB_CHUNK, b"loose chunk".to_vec())])
        .unwrap()[0];
    assert_eq!(s.blob_state(&chunk).unwrap(), BlobState::Missing);
    let b = block("b-00001", "claim", "alpha");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    let v = b.version_hash();
    assert!(s.has(&v).unwrap());
    assert_eq!(s.blob_state(&v).unwrap(), BlobState::Missing);
}

#[test]
fn dgm2_manifest_and_chunk_masks_return_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "claim", "alpha"),
            }],
        ),
    )
    .unwrap();
    let big: Vec<u8> = (0..MIB + 100).map(|i| (i % 241) as u8).collect();
    let x = s.put_blob(&big).unwrap();
    let mut shares = b"another head".to_vec();
    shares.resize(MIB, 7);
    shares.extend_from_slice(&big[MIB..]);
    let y = s.put_blob(&shares).unwrap();
    let z = s.put_blob(b"unrelated").unwrap();
    drop(s);
    // the first apply removes y: its manifest is masked, and x's shared second chunk
    let first = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![RedactTarget::Blob(y)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let r1 = first.requests[0].request;
    {
        let s = Store::open(&path).unwrap();
        // the manifest position
        assert_eq!(s.redaction_of(kinds::BLOB_MANIFEST, &y).unwrap(), Some(r1));
        assert_eq!(s.blob_state(&y).unwrap(), BlobState::Removed(r1));
        // the chunk position alone: x's manifest is unmasked
        assert_eq!(s.redaction_of(kinds::BLOB_MANIFEST, &x).unwrap(), None);
        assert_eq!(s.blob_state(&x).unwrap(), BlobState::Removed(r1));
        assert!(matches!(s.get_blob(&x), Err(StoreError::Redacted { request }) if request == r1));
        assert_eq!(s.blob_state(&z).unwrap(), BlobState::Readable);
    }
    // the second apply removes x: its manifest's mask is later than its chunk's
    let second = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![RedactTarget::Blob(x)]).at(41)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let r2 = second.requests[0].request;
    assert_ne!(r1, r2);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.redaction_of(kinds::BLOB_MANIFEST, &x).unwrap(), Some(r2));
    // the earliest authorizing apply wins, as a byte read names it
    assert_eq!(s.blob_state(&x).unwrap(), BlobState::Removed(r1));
    assert!(matches!(s.get_blob(&x), Err(StoreError::Redacted { request }) if request == r1));
    assert_eq!(s.blob_state(&y).unwrap(), BlobState::Removed(r1));
    assert_eq!(s.blob_state(&z).unwrap(), BlobState::Readable);
}

#[test]
fn dgm2_bad_manifest_remains_an_error() {
    let (_d, mut s) = store();
    let c = s.write(vec![(kinds::BLOB_CHUNK, vec![1])]).unwrap()[0];
    // a manifest with a trailing byte: not its own canonical encoding
    let mut bytes = postcard::to_allocvec(&BlobManifest {
        len: 1,
        chunks: vec![c],
    })
    .unwrap();
    bytes.push(0);
    let loose = s.write(vec![(kinds::BLOB_MANIFEST, bytes)]).unwrap()[0];
    let text = format!("{loose} is not a canonical blob manifest");
    assert_eq!(decode_text(s.blob_state(&loose)), text);
    assert_eq!(decode_text(s.get_blob(&loose)), text);
    // a manifest listing two chunks for one byte
    let two = s
        .put(
            kinds::BLOB_MANIFEST,
            &BlobManifest {
                len: 1,
                chunks: vec![c, c],
            },
        )
        .unwrap();
    let text = format!("blob {two} lists 2 chunks for 1 bytes");
    assert_eq!(decode_text(s.blob_state(&two)), text);
    assert_eq!(decode_text(s.get_blob(&two)), text);
}
