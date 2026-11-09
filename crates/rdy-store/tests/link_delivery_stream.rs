//! The link_delivery stream (repo links; D-DAPR8, D-FK58A): sync stream format 3 and its second
//! purpose. One `HEADER`, one stream-only `DELIVERY` (0xF4), the selected `BLOCK` objects in the
//! delivery's order (or their dropped representation), then one `TRAILER`. `HEADER.tag` is
//! provenance only: the stream carries no `TAG` and lists no prerequisite. The tag_history rules
//! and the released format-2 readers stay as they are.
use rdy_model::foreign::{
    ForeignDerived, ForeignPublication, ForeignVersion, SELECTION_IDENTITY_DOMAIN,
    selection_identity,
};
use rdy_model::{Block, BlockId, Hash, Provenance, RepoId, Role, Tag};
use rdy_store::bounded::MAX_RECORD_BYTES;
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::stream_source::CheckedStream;
use rdy_store::sync_stream::{
    DeliveryRecord, LINK_DELIVERY_FORMAT, MAGIC, STREAM_FORMAT, StreamHeader, StreamPurpose,
    StreamReader, StreamRecord, StreamWriter, check_batch, check_delivery, rec,
};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

const REPO: RepoId = RepoId([7; 16]);
/// The bytes of a frame header: length, kind and hash.
const HEAD: usize = 37;
/// The bytes of the prelude: the magic and the stream format.
const PRELUDE: u64 = 12;

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: Provenance {
            actor: "agent:w".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
    }
}

/// The delivered version of `b`, as the cited side states it.
fn version(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: b.version_hash(),
        content: b.content_hash(),
        encoding: 0,
        derived: ForeignDerived {
            status: Some("accepted".into()),
            lifecycle: None,
            superseded_by: vec![],
        },
    }
}

/// A delivery of `versions` in the given order, with their selection identity.
fn delivery_of(versions: Vec<ForeignVersion>) -> DeliveryRecord {
    let selection = selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap();
    DeliveryRecord {
        publication: ForeignPublication {
            tag: "v1".into(),
            profile: "pp-00001".into(),
            profile_version: Some(Hash([3; 32])),
        },
        selection,
        versions,
        shells: vec![],
    }
}

fn delivery(blocks: &[&Block]) -> DeliveryRecord {
    delivery_of(blocks.iter().map(|b| version(b)).collect())
}

/// `b` as a present BLOCK record, in its own encoding.
fn present(b: &Block) -> StreamRecord {
    let o = codec::encode_new(kinds::BLOCK, b);
    StreamRecord::Present {
        key: key(kinds::BLOCK, Hash::of(&o.payload)),
        encoding: o.version,
        payload: o.payload,
    }
}

fn header(tag: Hash) -> StreamHeader {
    StreamHeader::link_delivery(baseline_features(), REPO, tag)
}

/// One link_delivery stream: `h`, `d`, then `records`.
fn write(h: &StreamHeader, d: &DeliveryRecord, records: &[StreamRecord]) -> Vec<u8> {
    let mut w = StreamWriter::new_delivery(Vec::new(), h, d).unwrap();
    for r in records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32).to_le_bytes().to_vec();
    f.push(kind);
    f.extend_from_slice(&Hash::of(payload).0);
    f.extend_from_slice(payload);
    f
}

fn link(chain: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    chain.push(kind);
    chain.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    chain.extend_from_slice(&Hash::of(payload).0);
}

/// The header payload of a stream from the baseline store: `purpose`, `tag` and `prerequisites`.
fn header_payload(purpose: u8, tag: Hash, prerequisites: &[ObjectKey]) -> Vec<u8> {
    let json = baseline_features().to_json();
    let mut hp = (json.len() as u32).to_le_bytes().to_vec();
    hp.extend_from_slice(&json);
    hp.extend_from_slice(&REPO.0);
    hp.push(purpose);
    hp.extend_from_slice(&tag.0);
    hp.extend_from_slice(&(prerequisites.len() as u32).to_le_bytes());
    for k in prerequisites {
        hp.push(k.kind);
        hp.extend_from_slice(&k.hash.0);
    }
    hp
}

/// A stream of stream format `format`, built by hand: the header payload `hp`, then `frames` and
/// a trailer that states them, so only the rule under test is broken.
fn craft(format: u32, hp: &[u8], frames: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&format.to_le_bytes());
    out.extend(frame(rec::HEADER, hp));
    let mut chain = vec![];
    link(&mut chain, rec::HEADER, hp);
    for (kind, p) in frames {
        out.extend(frame(*kind, p));
        link(&mut chain, *kind, p);
    }
    let mut trailer = (frames.len() as u64).to_le_bytes().to_vec();
    trailer.extend_from_slice(&(out.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    out.extend(frame(rec::TRAILER, &trailer));
    out
}

fn payload(r: &StreamRecord) -> (u8, Vec<u8>) {
    match r {
        StreamRecord::Present { payload, .. } => (r.raw_kind(), payload.clone()),
        StreamRecord::Dropped { .. } => panic!("a present record"),
    }
}

fn enc(d: &DeliveryRecord) -> Vec<u8> {
    postcard::to_allocvec(d).unwrap()
}

/// The code, record, offset and message of a refusal.
fn refusal(e: StreamError) -> (&'static str, Option<u64>, u64, String) {
    (e.code, e.record, e.offset, e.message)
}

fn bad(record: u64, offset: u64, reason: &str) -> (&'static str, Option<u64>, u64, String) {
    (
        "E_BAD_RECORD",
        Some(record),
        offset,
        format!("stream record {record} at byte {offset}: {reason}"),
    )
}

/// The first byte of the frame after a header of payload `hp`.
fn after_header(hp: &[u8]) -> u64 {
    PRELUDE + (HEAD + hp.len()) as u64
}

#[test]
fn rpl6_delivery_has_one_header_manifest_and_trailer() {
    let (a, b) = (block("D-00001", "one"), block("D-00002", "two"));
    let d = delivery(&[&a, &b]);
    let root = Hash([9; 32]);
    let h = header(root);
    assert_eq!(LINK_DELIVERY_FORMAT, 3);
    assert_eq!(
        (h.stream_format, h.purpose, h.tag, h.prerequisites.len()),
        (3, StreamPurpose::LinkDelivery, root, 0)
    );
    assert_eq!(StreamPurpose::LinkDelivery.byte(), 1);
    assert_eq!(StreamPurpose::of(1), Some(StreamPurpose::LinkDelivery));
    assert_eq!(StreamPurpose::of(2), None);
    assert_eq!(rec::DELIVERY, 0xF4);
    let records = vec![present(&a), present(&b)];
    let bytes = write(&h, &d, &records);

    // the bytes, field by field: format 3, the header with its manifest and purpose 1, one
    // DELIVERY, the BLOCK objects, and the trailer, whose digest covers the DELIVERY
    let hp = header_payload(1, root, &[]);
    let dp = enc(&d);
    let mut want = MAGIC.to_vec();
    want.extend_from_slice(&3u32.to_le_bytes());
    want.extend(frame(0xF0, &hp));
    want.extend(frame(0xF4, &dp));
    let mut chain = vec![];
    link(&mut chain, 0xF0, &hp);
    link(&mut chain, 0xF4, &dp);
    for r in &records {
        let (kind, p) = payload(r);
        want.extend(frame(kind, &p));
        link(&mut chain, kind, &p);
    }
    let before = want.len() as u64;
    let digest = Hash::of(&chain);
    let mut trailer = 3u64.to_le_bytes().to_vec();
    trailer.extend_from_slice(&before.to_le_bytes());
    trailer.extend_from_slice(&digest.0);
    want.extend(frame(0xFF, &trailer));
    assert_eq!(bytes, want, "stream format 3, byte for byte");

    // the check: the header, the delivery, the selected keys in wire order and the totals
    let report = check_delivery(&bytes[..]).unwrap();
    assert_eq!(report.header, h);
    assert_eq!(report.delivery, d);
    assert_eq!(
        report.keys,
        vec![
            key(kinds::BLOCK, a.version_hash()),
            key(kinds::BLOCK, b.version_hash())
        ]
    );
    assert_eq!(
        (report.records, report.bytes, report.digest),
        (3, before, digest)
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["header"]["purpose"], "link_delivery");
    assert_eq!(json["header"]["stream_format"], 3);
    assert_eq!(json["header"]["prerequisites"], serde_json::json!([]));
    assert_eq!(json["delivery"]["selection"], d.selection.to_string());
    assert_eq!(
        json["delivery"]["versions"][0]["version"],
        a.version_hash().to_string()
    );

    // the reader reads the DELIVERY with the header, as record 1; the objects follow it
    let (mut r, read) = StreamReader::open(&bytes[..]).unwrap();
    assert_eq!(read, h);
    assert_eq!(r.delivery(), Some(&d));
    assert_eq!(r.position(), (1, after_header(&hp)));
    assert_eq!(r.next().unwrap(), Some(records[0].clone()));
    assert_eq!(r.next().unwrap(), Some(records[1].clone()));
    assert_eq!(r.next().unwrap(), None);
    // a tag history has no DELIVERY
    let th = StreamHeader::tag_history(baseline_features(), REPO, root, vec![]);
    let plain = craft(2, &header_payload(0, root, &[]), &[]);
    let (r, _) = StreamReader::open(&plain[..]).unwrap();
    assert_eq!(r.delivery(), None);
    assert_eq!(th.stream_format, STREAM_FORMAT);

    // an empty selection is one DELIVERY and no object
    let none = delivery(&[]);
    let empty = write(&h, &none, &[]);
    let report = check_delivery(&empty[..]).unwrap();
    assert_eq!((report.records, report.keys.len()), (1, 0));
    assert_eq!(
        none.selection,
        Hash::of(
            &[
                SELECTION_IDENTITY_DOMAIN,
                &1u32.to_le_bytes(),
                &0u32.to_le_bytes()
            ]
            .concat()
        )
    );

    // exactly one DELIVERY, right after the header: none, a late one or a second one refuses
    let at = after_header(&hp);
    let (ap, bp) = (payload(&records[0]), payload(&records[1]));
    let missing = craft(3, &hp, &[ap.clone(), bp.clone()]);
    assert_eq!(
        refusal(check_delivery(&missing[..]).unwrap_err()),
        bad(1, at, "delivery record out of order")
    );
    let bare = craft(3, &hp, &[]);
    assert_eq!(
        refusal(check_delivery(&bare[..]).unwrap_err()),
        bad(1, at, "delivery record out of order")
    );
    let second_at = at + (HEAD + dp.len()) as u64;
    let twice = craft(
        3,
        &hp,
        &[(0xF4, dp.clone()), (0xF4, dp.clone()), ap.clone()],
    );
    assert_eq!(
        refusal(check_delivery(&twice[..]).unwrap_err()),
        bad(2, second_at, "delivery record out of order")
    );
    let late = craft(
        3,
        &hp,
        &[(0xF4, dp.clone()), ap.clone(), (0xF4, dp.clone())],
    );
    assert_eq!(
        check_delivery(&late[..]).unwrap_err().message,
        format!(
            "stream record 3 at byte {}: delivery record out of order",
            second_at + (HEAD + ap.1.len()) as u64
        )
    );
    // and a DELIVERY in a tag history is no object kind
    let th_hp = header_payload(0, root, &[]);
    let stray = craft(2, &th_hp, &[(0xF4, dp.clone())]);
    assert_eq!(
        refusal(check_batch(&stray[..]).unwrap_err()),
        bad(1, after_header(&th_hp), "unknown kind")
    );
    // the trailer ends the file
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        refusal(check_delivery(&trailing[..]).unwrap_err()),
        bad(5, bytes.len() as u64, "trailing bytes")
    );
    // the trailer's digest covers the DELIVERY
    let mut other = d.clone();
    other.publication.tag = "v2".into();
    let mut swapped = bytes.clone();
    let swapped_dp = enc(&other);
    assert_eq!(swapped_dp.len(), dp.len());
    let start = at as usize;
    swapped.splice(start..start + HEAD + dp.len(), frame(0xF4, &swapped_dp));
    assert_eq!(
        check_delivery(&swapped[..]).unwrap_err().code,
        "E_STREAM_TRUNCATED"
    );
}

#[test]
fn rpl6_tag_is_provenance_not_root() {
    let a = block("D-00001", "one");
    let d = delivery(&[&a]);
    let t = tag("v1", Hash([4; 32]));
    let root = t.id();
    let h = header(root);
    // the header names the cited TAG; the stream holds no TAG and needs no prerequisite
    let bytes = write(&h, &d, &[present(&a)]);
    let report = check_delivery(&bytes[..]).unwrap();
    assert_eq!(report.header.tag, root);
    assert!(report.header.prerequisites.is_empty());
    assert!(!report.keys.contains(&key(kinds::TAG, root)));
    assert_eq!(report.keys, vec![key(kinds::BLOCK, a.version_hash())]);

    // a TAG is no object of this purpose: the writer and the reader refuse it
    let hp = header_payload(1, root, &[]);
    let dp = enc(&d);
    let tag_payload = postcard::to_allocvec(&t).unwrap();
    let tag_record = StreamRecord::Present {
        key: key(kinds::TAG, root),
        encoding: 0,
        payload: tag_payload.clone(),
    };
    let mut w = StreamWriter::new_delivery(Vec::new(), &h, &d).unwrap();
    let at = after_header(&hp) + (HEAD + dp.len()) as u64;
    let e = w.record(&tag_record).unwrap_err();
    assert_eq!(refusal(e), bad(2, at, "unknown kind"));
    let with_tag = craft(
        3,
        &hp,
        &[(0xF4, dp.clone()), (kinds::TAG, tag_payload.clone())],
    );
    assert_eq!(
        refusal(check_delivery(&with_tag[..]).unwrap_err()),
        bad(2, at, "unknown kind")
    );
    // nor any other object kind
    let other = StreamRecord::Present {
        key: key(kinds::LANDING, Hash::of(b"x")),
        encoding: 0,
        payload: b"x".to_vec(),
    };
    let mut w = StreamWriter::new_delivery(Vec::new(), &h, &d).unwrap();
    assert_eq!(
        refusal(w.record(&other).unwrap_err()),
        bad(2, at, "unknown kind")
    );

    // nor a prerequisite: a header that lists the TAG, or any key, is invalid
    let listed = StreamHeader {
        prerequisites: vec![key(kinds::TAG, root)],
        ..h.clone()
    };
    let Err(e) = StreamWriter::new_delivery(Vec::new(), &listed, &d) else {
        panic!("the writer refuses a prerequisite")
    };
    assert_eq!(refusal(e), bad(0, PRELUDE, "invalid header"));
    let listed_hp = header_payload(1, root, &[key(kinds::TAG, root)]);
    let crafted = craft(3, &listed_hp, &[(0xF4, dp.clone()), payload(&present(&a))]);
    assert_eq!(
        refusal(check_delivery(&crafted[..]).unwrap_err()),
        bad(0, PRELUDE, "invalid header")
    );

    // the tag_history rule stays: a batch neither holding nor listing its root TAG is refused
    let th = StreamHeader::tag_history(baseline_features(), REPO, root, vec![]);
    let mut w = StreamWriter::new(Vec::new(), &th).unwrap();
    w.record(&present(&a)).unwrap();
    let batch = w.finish().unwrap().0;
    assert_eq!(
        refusal(check_batch(&batch[..]).unwrap_err()),
        bad(
            0,
            PRELUDE,
            "root tag is not in the batch or its prerequisites"
        )
    );
}

#[test]
fn rpl6_wrong_purpose_and_bad_selection_refuse() {
    let (a, b) = (block("D-00001", "one"), block("D-00002", "two"));
    let root = Hash([9; 32]);
    let h = header(root);
    let d = delivery(&[&a, &b]);
    let bytes = write(&h, &d, &[present(&a), present(&b)]);
    let th = StreamHeader::tag_history(baseline_features(), REPO, root, vec![]);
    let batch = craft(2, &header_payload(0, root, &[]), &[]);

    // the wrong purpose: each check names the purpose it expects, at the header
    let e = check_batch(&bytes[..]).unwrap_err();
    assert_eq!(
        (
            e.code,
            e.record,
            e.offset,
            e.message.as_str(),
            e.fix.as_str()
        ),
        (
            "E_STREAM_PURPOSE",
            Some(0),
            PRELUDE,
            "stream purpose 1 does not match tag_history",
            "request a tag_history stream"
        )
    );
    let e = check_delivery(&batch[..]).unwrap_err();
    assert_eq!(
        (
            e.code,
            e.record,
            e.offset,
            e.message.as_str(),
            e.fix.as_str()
        ),
        (
            "E_STREAM_PURPOSE",
            Some(0),
            PRELUDE,
            "stream purpose 0 does not match link_delivery",
            "request a link_delivery stream"
        )
    );
    let e = StreamReader::open_for(&bytes[..], StreamPurpose::TagHistory)
        .err()
        .unwrap();
    assert_eq!(e.code, "E_STREAM_PURPOSE");
    // a checked tag-history source refuses a delivery the same way
    let e = CheckedStream::open(Cursor::new(bytes.clone()))
        .err()
        .unwrap();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 1 does not match tag_history"
        )
    );
    // the writers too
    let Err(e) = StreamWriter::new(Vec::new(), &h) else {
        panic!("a tag-history writer refuses a delivery header")
    };
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 1 does not match tag_history"
        )
    );
    let Err(e) = StreamWriter::new_delivery(Vec::new(), &th, &d) else {
        panic!("a delivery writer refuses a tag-history header")
    };
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 0 does not match link_delivery"
        )
    );
    // an unknown purpose byte names the purpose the check expects
    let e = check_delivery(&craft(3, &header_payload(7, root, &[]), &[])[..]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 7 does not match link_delivery"
        )
    );

    // a bad selection: refused at the DELIVERY by the writer and by the reader
    let hp = header_payload(1, root, &[]);
    let at = after_header(&hp);
    let (ap, bp) = (payload(&present(&a)), payload(&present(&b)));
    let mut wrong_digest = d.clone();
    wrong_digest.selection = Hash([1; 32]);
    let unsorted = delivery_of(vec![version(&b), version(&a)]);
    let repeated = delivery_of(vec![version(&a), version(&a)]);
    // two ids cannot name one BLOCK: a dropped record could not tell them apart
    let shared = delivery_of(vec![
        version(&a),
        ForeignVersion {
            version: a.version_hash(),
            ..version(&b)
        },
    ]);
    let bad_encoding = delivery_of(vec![ForeignVersion {
        encoding: 9,
        ..version(&a)
    }]);
    for (what, sel, reason) in [
        ("digest", &wrong_digest, "invalid delivered selection"),
        ("order", &unsorted, "invalid delivered selection"),
        ("repeat", &repeated, "invalid delivered selection"),
        ("shared version", &shared, "invalid delivered selection"),
        ("encoding", &bad_encoding, "unknown encoding version"),
    ] {
        let Err(e) = StreamWriter::new_delivery(Vec::new(), &h, sel) else {
            panic!("{what}: the writer refuses it")
        };
        assert_eq!(refusal(e), bad(1, at, reason), "{what}");
        let crafted = craft(3, &hp, &[(0xF4, enc(sel)), ap.clone(), bp.clone()]);
        assert_eq!(
            refusal(check_delivery(&crafted[..]).unwrap_err()),
            bad(1, at, reason),
            "{what}"
        );
    }

    // the objects must be exactly the selection, in its order
    let dp = enc(&d);
    let first = at + (HEAD + dp.len()) as u64;
    let second = first + (HEAD + ap.1.len()) as u64;
    // a missing version: the DELIVERY needs it
    let short = craft(3, &hp, &[(0xF4, dp.clone()), ap.clone()]);
    let e = check_delivery(&short[..]).unwrap_err();
    assert_eq!(
        (e.code, e.record, e.offset, e.message.clone()),
        (
            "E_UNRESOLVED_REF",
            Some(1),
            at,
            format!("stream record 1 needs 1:{}", b.version_hash())
        )
    );
    assert_eq!(e.object, Some(key(kinds::BLOCK, b.version_hash())));
    // the wrong order, and an object beyond the selection
    let swapped = craft(3, &hp, &[(0xF4, dp.clone()), bp.clone(), ap.clone()]);
    let e = check_delivery(&swapped[..]).unwrap_err();
    assert_eq!(
        refusal(e.clone()),
        bad(2, first, "object is outside the delivered selection")
    );
    assert_eq!(e.object, Some(key(kinds::BLOCK, b.version_hash())));
    let only_a = enc(&delivery(&[&a]));
    let extra = craft(3, &hp, &[(0xF4, only_a.clone()), ap.clone(), bp.clone()]);
    let first_a = at + (HEAD + only_a.len()) as u64;
    assert_eq!(
        refusal(check_delivery(&extra[..]).unwrap_err()),
        bad(
            3,
            first_a + (HEAD + ap.1.len()) as u64,
            "object is outside the delivered selection"
        )
    );
    // a version of another encoding, or of another id, than the DELIVERY states
    let one = delivery_of(vec![ForeignVersion {
        encoding: 1,
        ..version(&a)
    }]);
    let other_id = delivery_of(vec![ForeignVersion {
        id: BlockId::parse("D-00003").unwrap(),
        ..version(&a)
    }]);
    for (what, sel) in [("encoding", one), ("id", other_id)] {
        let sp = enc(&sel);
        let crafted = craft(3, &hp, &[(0xF4, sp.clone()), ap.clone()]);
        assert_eq!(
            refusal(check_delivery(&crafted[..]).unwrap_err()),
            bad(
                2,
                at + (HEAD + sp.len()) as u64,
                "object is outside the delivered selection"
            ),
            "{what}"
        );
    }
    assert!(second > first);

    // a redacted version travels as its residue: a dropped BLOCK, never a dropped other kind
    let dropped_of = |k: ObjectKey, residue: Option<Hash>| StreamRecord::Dropped {
        key: k,
        encoding: 0,
        original_len: 40,
        apply: Hash([22; 32]),
        residue,
    };
    let dropped = dropped_of(key(kinds::BLOCK, b.version_hash()), Some(Hash([21; 32])));
    let report = check_delivery(&write(&h, &d, &[present(&a), dropped.clone()])[..]).unwrap();
    assert_eq!(report.keys[1], key(kinds::BLOCK, b.version_hash()));
    let no_residue = dropped_of(key(kinds::BLOCK, b.version_hash()), None);
    let mut w = StreamWriter::new_delivery(Vec::new(), &h, &d).unwrap();
    w.record(&present(&a)).unwrap();
    assert_eq!(
        refusal(w.record(&no_residue).unwrap_err()),
        bad(3, second, "invalid dropped record")
    );
    let change = dropped_of(key(kinds::CHANGE, Hash([5; 32])), Some(Hash([21; 32])));
    let mut w = StreamWriter::new_delivery(Vec::new(), &h, &d).unwrap();
    assert_eq!(
        refusal(w.record(&change).unwrap_err()),
        bad(2, first, "unknown kind")
    );

    // the bounds, before allocation: a DELIVERY over the record bytes, and more than 100000
    // selected versions
    let mut long = MAGIC.to_vec();
    long.extend_from_slice(&3u32.to_le_bytes());
    long.extend(frame(0xF0, &hp));
    long.extend_from_slice(&(MAX_RECORD_BYTES as u32 + 1).to_le_bytes());
    long.push(0xF4);
    long.extend_from_slice(&[0u8; 32]);
    let e = check_delivery(&long[..]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "stream exceeds 16777216 record bytes")
    );
    // the count is refused before any other rule, so one id serves
    let many: Vec<ForeignVersion> = (0..100_001u32)
        .map(|_| ForeignVersion {
            id: BlockId::parse("D-00001").unwrap(),
            version: Hash([1; 32]),
            content: Hash([2; 32]),
            encoding: 0,
            derived: ForeignDerived::default(),
        })
        .collect();
    let big = delivery_of(many);
    let Err(e) = StreamWriter::new_delivery(Vec::new(), &h, &big) else {
        panic!("the writer refuses 100001 versions")
    };
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "stream exceeds 100000 collection elements")
    );
    let crafted = craft(3, &hp, &[(0xF4, enc(&big))]);
    let e = check_delivery(&crafted[..]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "stream exceeds 100000 collection elements")
    );
    // a DELIVERY that does not decode
    let garbage = craft(3, &hp, &[(0xF4, vec![0xFF; 9])]);
    assert_eq!(
        refusal(check_delivery(&garbage[..]).unwrap_err()),
        bad(1, at, "record does not decode")
    );
}

fn sync_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../rdy-qa/fixtures/sync/{name}"))
}

#[test]
fn rpl6_old_sync_fixtures_still_read() {
    // every released sync batch (format 2, from Release 1) reads as it did
    for name in ["v2-f1-e0", "v2-f1-e1", "v2-f1-e1-ck"] {
        let bytes = std::fs::read(sync_fixture(&format!("{name}.rdysync"))).unwrap();
        assert_eq!(bytes[8..12], 2u32.to_le_bytes(), "{name}");
        let report = check_batch(&bytes[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(report.header.stream_format, 2, "{name}");
        assert_eq!(report.header.purpose, StreamPurpose::TagHistory, "{name}");
        // the format-3 container reads the same tag history: only the prelude differs
        let mut three = bytes.clone();
        three[8..12].copy_from_slice(&3u32.to_le_bytes());
        let again = check_batch(&three[..]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(again.header.stream_format, 3, "{name}");
        assert_eq!(
            StreamHeader {
                stream_format: 2,
                ..again.header.clone()
            },
            report.header,
            "{name}"
        );
        assert_eq!(
            (again.keys, again.records, again.bytes, again.digest),
            (report.keys, report.records, report.bytes, report.digest),
            "{name}"
        );
        // a released batch is no delivery
        assert_eq!(
            check_delivery(&bytes[..]).unwrap_err().code,
            "E_STREAM_PURPOSE",
            "{name}"
        );
    }
    // format 1 stays refused by its own text, and an unknown format by the header
    let one = std::fs::read(sync_fixture("v1-s15.rdysync")).unwrap();
    let e = check_batch(&one[..]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_STORE_FORMAT", "sync batch format 1 is not supported")
    );
    let root = Hash([9; 32]);
    let four = craft(4, &header_payload(0, root, &[]), &[]);
    assert_eq!(
        refusal(check_batch(&four[..]).unwrap_err()),
        bad(0, 8, "invalid header")
    );
    // format 2 knows only tag_history, as the released reader does
    let e = check_delivery(&craft(2, &header_payload(1, root, &[]), &[])[..]).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 1 does not match tag_history"
        )
    );
    // a tag history is still written in the released format 2, so a Release 1 reader takes it
    assert_eq!(STREAM_FORMAT, 2);
    let th = StreamHeader::tag_history(baseline_features(), REPO, root, vec![]);
    assert_eq!(th.stream_format, 2);
    let (written, _) = StreamWriter::new(Vec::new(), &th)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(written[8..12], 2u32.to_le_bytes());
}
