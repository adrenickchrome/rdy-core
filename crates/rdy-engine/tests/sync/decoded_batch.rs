//! One decoded value per checked record (D-3PNRK, D-D2NPN verify-on-receive): the engine's
//! local limits read the value the store check decoded, and run before any receipt exists.
use rdy_engine::publication::{self, Limits};
use rdy_engine::stream_check::{CheckedBatch, check_batch, check_batch_prepared_tally};
use rdy_engine::{E_BAD_RECORD, E_LIMIT, RdyError};
use rdy_model::*;
use rdy_store::bounded;
use rdy_store::codec;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{self, StreamHeader, StreamReader, StreamRecord, StreamWriter};
use std::collections::BTreeMap;
use std::path::Path;

/// The released history fixtures this rdy reads, by file name.
const FIXTURES: [&str; 7] = [
    "v2-f1-e0.rdysync",
    "v2-f1-e1.rdysync",
    "v2-f1-e1-ck.rdysync",
    "v2-f1-e1-snapshot-delta.rdysync",
    "sync4-canon-history.bin",
    "sync4-view-history.bin",
    "sync4-reduction.bin",
];

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rdy-qa/fixtures/sync")
            .join(name),
    )
    .unwrap()
}

/// The present records of `bytes` that hold a typed value: all but blob chunks.
fn typed_records(bytes: &[u8]) -> u64 {
    let (mut r, _) = StreamReader::open(bytes).unwrap();
    let mut n = 0;
    while let Some(rec) = r.next().unwrap() {
        if matches!(&rec, StreamRecord::Present { key, .. } if key.kind != kinds::BLOB_CHUNK) {
            n += 1;
        }
    }
    n
}

/// The TAG key a crafted batch names as its absent root.
fn root() -> ObjectKey {
    ObjectKey {
        kind: kinds::TAG,
        hash: Hash([0x11; 32]),
    }
}

/// `records` as one batch of repo 7s rooted at [`root`], whose prerequisites are the root and
/// `extra`.
fn crafted(records: &[StreamRecord], extra: &[ObjectKey]) -> Vec<u8> {
    let mut pre: Vec<ObjectKey> = extra.to_vec();
    pre.push(root());
    pre.sort();
    pre.dedup();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        RepoId([7; 16]),
        root().hash,
        pre,
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for r in records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

/// A present record of `kind` holding `value`, encoded as rdy stores it.
fn record<T: serde::Serialize + 'static>(kind: u8, value: &T) -> StreamRecord {
    let (raw, payload) = codec::item_new(kind, value);
    StreamRecord::Present {
        key: ObjectKey {
            kind: codec::logical_kind(raw),
            hash: Hash::of(&payload),
        },
        encoding: 0,
        payload,
    }
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "prose".into(),
        parent: None,
        order: "a".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn chunk_keys(m: &BlobManifest) -> Vec<ObjectKey> {
    m.chunks
        .iter()
        .map(|h| ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: *h,
        })
        .collect()
}

fn prepared(bytes: &[u8]) -> Result<CheckedBatch, RdyError> {
    check_batch_prepared_tally(bytes, None, |_, _, _| {})
}

/// `bytes` is refused by the prepared check, with no capsule, exactly as [`check_batch`] refuses
/// it: the same code, message, fix and positioned issue.
fn refused_alike(bytes: &[u8]) -> RdyError {
    let old = check_batch(bytes, None).unwrap_err();
    let new = prepared(bytes).unwrap_err();
    assert_eq!(
        serde_json::to_value(&new).unwrap(),
        serde_json::to_value(&old).unwrap()
    );
    new
}

/// The one positioned issue a stream refusal carries.
fn position(e: &RdyError) -> String {
    assert_eq!(e.issues.len(), 1, "{e:?}");
    e.issues[0].message.clone()
}

#[test]
fn r59010_engine_limits_still_precede_receipt() {
    // the engine's own limits read the store check's one value; a good batch's capsule reports
    // what the check always reported
    for name in FIXTURES {
        let bytes = fixture(name);
        let want = typed_records(&bytes);
        bounded::reset_decodes_for_tests();
        let old = check_batch(&bytes[..], None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(bounded::decodes_for_tests(), want, "{name}");
        bounded::reset_decodes_for_tests();
        let mut tallied = 0u64;
        let ready = check_batch_prepared_tally(&bytes[..], None, |_, _, _| tallied += 1)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(bounded::decodes_for_tests(), want, "{name}: prepared");
        assert_eq!(ready.report(), &old, "{name}");
        assert_eq!(ready.store_checked().report(), &old, "{name}");
        assert_eq!(ready.store_checked().input_len(), bytes.len() as u64);
        assert_eq!(
            tallied, old.records,
            "{name}: one tally per record, as before"
        );
        assert_eq!(ready.into_report(), old, "{name}");
    }

    // an oversized BLOCK text: refused at the record, by the publication limit, with no capsule
    let text = "x".repeat(48);
    let rec = record(kinds::BLOCK, &block("p-TEXT1", &text));
    let key = rec.key();
    let b = crafted(&[rec], &[]);
    let head = crafted(&[], &[]).len() - 37 - 48;
    let fits = Limits {
        text: 48,
        ..Limits::default()
    };
    {
        let _g = publication::override_limits_for_tests(fits);
        prepared(&b).unwrap();
    }
    let over = Limits {
        text: 47,
        ..Limits::default()
    };
    {
        let _g = publication::override_limits_for_tests(over);
        sync_stream::check_batch(b.as_slice()).unwrap();
        let e = refused_alike(&b);
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                E_LIMIT,
                "Publication text exceeds the limit of 47 bytes.",
                "Reduce the selection or split the source project."
            )
        );
        assert_eq!(
            position(&e),
            format!("record 1 at byte {head}; object {key}")
        );
    }

    // hostile blob manifests: the store check passes them; the engine refuses each at its
    // record before any capsule exists
    let max = 67_108_864u64;
    let chunks = |n: u8| -> Vec<Hash> { (0..n).map(|i| Hash([i + 1; 32])).collect() };
    let fine = BlobManifest {
        len: max,
        chunks: chunks(64),
    };
    let rec = record(kinds::BLOB_MANIFEST, &fine);
    prepared(&crafted(&[rec], &chunk_keys(&fine))).unwrap();
    let over = BlobManifest {
        len: max + 1,
        chunks: chunks(65),
    };
    let rec = record(kinds::BLOB_MANIFEST, &over);
    let key = rec.key();
    let b = crafted(&[rec], &chunk_keys(&over));
    sync_stream::check_batch(b.as_slice()).unwrap();
    let e = refused_alike(&b);
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_LIMIT,
            "Publication blob exceeds the limit of 67108864 bytes."
        )
    );
    let head_65 = crafted(&[], &chunk_keys(&over)).len() - 37 - 48;
    assert_eq!(
        position(&e),
        format!("record 1 at byte {head_65}; object {key}")
    );
    for wrong in [
        BlobManifest {
            len: 5,
            chunks: chunks(2),
        },
        BlobManifest {
            len: (1 << 20) + 1,
            chunks: chunks(1),
        },
        BlobManifest {
            len: 0,
            chunks: chunks(1),
        },
    ] {
        let rec = record(kinds::BLOB_MANIFEST, &wrong);
        let b = crafted(&[rec], &chunk_keys(&wrong));
        sync_stream::check_batch(b.as_slice()).unwrap();
        let e = refused_alike(&b);
        assert_eq!(e.code, E_BAD_RECORD, "{wrong:?}");
        assert!(e.message.ends_with(": record does not decode"), "{e}");
    }
    // a manifest that decodes but is not its own canonical encoding
    let payload = vec![0x80, 0x00, 0x00];
    let key = ObjectKey {
        kind: kinds::BLOB_MANIFEST,
        hash: Hash::of(&payload),
    };
    let b = crafted(
        &[StreamRecord::Present {
            encoding: 0,
            key,
            payload,
        }],
        &[],
    );
    sync_stream::check_batch(b.as_slice()).unwrap();
    let e = refused_alike(&b);
    assert_eq!(
        e.message,
        format!("stream record 1 at byte {head}: record does not decode")
    );
    assert_eq!(
        position(&e),
        format!("record 1 at byte {head}; object {key}")
    );

    // a store refusal stays the store's: no capsule, the same error
    let mut cut = fixture("v2-f1-e1.rdysync");
    cut.pop();
    refused_alike(&cut);
}
