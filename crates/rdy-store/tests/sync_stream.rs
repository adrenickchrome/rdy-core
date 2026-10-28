//! The sync batch (plan HA1 Task 5; D-D2NPN: "records as store frames, header, trailer,
//! dependency order, verify-on-receive"; D-DAGNB: the repo id; hub i-QD955, p-97JVE): the exact
//! bytes of stream format 1, batches that stand alone (a late root, a zero-object batch, their
//! own prerequisites and trailers), the exact local prerequisite rule, refused damage at its
//! first record and byte, and every count and byte bound at N and N+1.
use rdy_model::{
    Block, BlockId, Hash, Intent, IntentStatus, Provenance, RepoId, Role, StateSnapshot, Tag,
};
use rdy_store::dump::{DumpBatch, DumpHeader, DumpWriter, Frame, RemovalFacts};
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader};
use rdy_store::format::baseline_features;
use rdy_store::kinds::{self, batch_digest};
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{
    BatchReport, LIMIT_BATCH_FILES, LIMIT_DIGEST_BYTES, MAGIC, MAX_BATCH_FILES, MAX_RECORDS,
    MAX_STREAM_BYTES, StreamHeader, StreamPurpose, StreamReader, StreamRecord, StreamWriter,
    check_batch,
};
use std::collections::BTreeMap;
use std::io::Read;

const REPO: RepoId = RepoId([7; 16]);
const HEAD: usize = 37;
const PRELUDE: u64 = 12;
/// The bytes of the baseline manifest's JSON in a header.
fn manifest_len() -> usize {
    baseline_features().to_json().len()
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:w".into(),
        run: None,
        role: Role::Writer,
        at_ms: 1,
    }
}

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
        read_identity: None,
    }
}

fn tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov(),
    }
}

fn intent(prev: Option<Hash>, n: u64) -> Intent {
    Intent {
        stable: None,
        prev,
        actor: "agent:w".into(),
        run: None,
        view: "work".into(),
        area: None,
        blocks: vec![],
        angle: format!("step {n}"),
        expires_at_ms: 1,
        status: IntentStatus::Open,
        at_ms: n as i64,
    }
}

/// A present record of `kind` holding `value`.
fn present<T: serde::Serialize>(kind: u8, value: &T) -> StreamRecord {
    let payload = enc(value);
    StreamRecord::Present {
        encoding: 0,
        key: key(kind, Hash::of(&payload)),
        payload,
    }
}

fn raw(kind: u8, payload: &[u8]) -> StreamRecord {
    StreamRecord::Present {
        encoding: 0,
        key: key(kind, Hash::of(payload)),
        payload: payload.to_vec(),
    }
}

fn header(tag: Hash, prerequisites: Vec<ObjectKey>) -> StreamHeader {
    StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        REPO,
        tag,
        prerequisites,
    )
}

/// `records` written as one batch with `header`.
fn batch(header: &StreamHeader, records: &[StreamRecord]) -> Vec<u8> {
    let mut w = StreamWriter::new(Vec::new(), header).unwrap();
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

fn check(bytes: &[u8]) -> Result<BatchReport, StreamError> {
    check_batch(bytes)
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

/// A frame header claiming `len` payload bytes of `kind`, with any hash.
fn frame_head(kind: u8, len: u32) -> Vec<u8> {
    let mut f = len.to_le_bytes().to_vec();
    f.push(kind);
    f.extend_from_slice(&[0u8; 32]);
    f
}

/// The first `n` bytes of a batch with no object records, then `tail`: the prelude and the
/// header of `h`.
fn opened(h: &StreamHeader) -> Vec<u8> {
    let full = batch(h, &[]);
    full[..full.len() - HEAD - 48].to_vec()
}

#[test]
fn ha5_stream_one_has_exact_header_frames_and_trailer() {
    let landing = Hash([4; 32]);
    let b = block("b-00001", "kept");
    let t = tag("v1", landing);
    let chunk = Hash([6; 32]);
    let apply = Hash([22; 32]);
    let records = vec![
        present(kinds::BLOCK, &b),
        StreamRecord::Dropped {
            encoding: 0,
            key: key(kinds::BLOB_CHUNK, chunk),
            original_len: 5,
            apply,
            residue: None,
        },
        present(kinds::TAG, &t),
    ];
    let root = t.id();
    // the tag needs its landing; the absent apply of the dropped chunk is a prerequisite too
    let h = header(
        root,
        vec![
            key(kinds::LANDING, landing),
            key(kinds::REDACTION_APPLY, apply),
        ],
    );
    let bytes = batch(&h, &records);

    // the bytes, field by field
    let json = baseline_features().to_json();
    let mut hp = (json.len() as u32).to_le_bytes().to_vec();
    hp.extend_from_slice(&json);
    hp.extend_from_slice(&REPO.0);
    hp.push(0);
    hp.extend_from_slice(&root.0);
    hp.extend_from_slice(&2u32.to_le_bytes());
    hp.push(kinds::LANDING);
    hp.extend_from_slice(&landing.0);
    hp.push(kinds::REDACTION_APPLY);
    hp.extend_from_slice(&apply.0);
    assert_eq!(hp.len(), 57 + json.len() + 2 * 33);
    let mut dropped = vec![kinds::BLOB_CHUNK, 0, 0];
    dropped.extend_from_slice(&chunk.0);
    dropped.extend_from_slice(&5u32.to_le_bytes());
    dropped.extend_from_slice(&apply.0);
    dropped.push(0);
    dropped.extend_from_slice(&[0u8; 32]);
    assert_eq!(dropped.len(), 104);
    let (bp, tp) = (enc(&b), enc(&t));
    let mut want = MAGIC.to_vec();
    want.extend_from_slice(&2u32.to_le_bytes());
    want.extend(frame(0xF0, &hp));
    want.extend(frame(kinds::BLOCK, &bp));
    want.extend(frame(0xF3, &dropped));
    want.extend(frame(kinds::TAG, &tp));
    let before = want.len() as u64;
    // the dump recipe: kind || len (u64) || hash of the header and every object frame
    let mut chain = vec![];
    link(&mut chain, 0xF0, &hp);
    link(&mut chain, kinds::BLOCK, &bp);
    link(&mut chain, 0xF3, &dropped);
    link(&mut chain, kinds::TAG, &tp);
    let digest = Hash::of(&chain);
    let mut trailer = 3u64.to_le_bytes().to_vec();
    trailer.extend_from_slice(&before.to_le_bytes());
    trailer.extend_from_slice(&digest.0);
    want.extend(frame(0xFF, &trailer));
    assert_eq!(bytes, want, "stream format 2, byte for byte");

    // the DROPPED body is dump format 4's, byte for byte
    let mut dump = DumpWriter::new(
        Vec::new(),
        &DumpHeader {
            store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
            log_end: Location {
                segment: 0,
                offset: 0,
            },
            rdy: String::new(),
            chain: Some(DumpChainHeader::full(RepoId([0; 16]), BatchMarker::ZERO)),
        },
    )
    .unwrap();
    dump.batch(&DumpBatch {
        start: Location {
            segment: 0,
            offset: 0,
        },
        digest: batch_digest([&chunk]),
        frames: vec![Frame::Dropped {
            kind: kinds::BLOB_CHUNK,
            hash: chunk,
            len: 5,
            apply,
            residue: None,
        }],
    })
    .unwrap();
    let dump = dump.finish(RemovalFacts::default()).unwrap().0;
    let dumped = frame(0xF3, &dropped);
    assert!(
        dump.windows(dumped.len()).any(|w| w == dumped.as_slice()),
        "the dump holds the same DROPPED record"
    );

    // the reader gives the records back, then the trailer's statement
    let (mut r, read) = StreamReader::open(&bytes[..]).unwrap();
    assert_eq!(read, h);
    let mut got = vec![];
    while let Some(rec) = r.next().unwrap() {
        got.push(rec);
    }
    assert_eq!(got, records);
    let totals = r.finish().unwrap();
    assert_eq!(
        (totals.records, totals.bytes, totals.digest),
        (3, before, digest)
    );

    // the report: keys in wire order, the trailer's numbers, hex JSON
    let report = check(&bytes).unwrap();
    assert_eq!(
        report.keys,
        vec![
            key(kinds::BLOCK, b.version_hash()),
            key(kinds::BLOB_CHUNK, chunk),
            key(kinds::TAG, root),
        ]
    );
    assert_eq!(
        (report.records, report.bytes, report.digest),
        (3, before, digest)
    );
    assert_eq!(report.header, h);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["header"],
        serde_json::json!({
            "stream_format": 2,
            "features": serde_json::to_value(rdy_store::format::baseline_features()).unwrap(),
            "repo_id": "07070707070707070707070707070707",
            "purpose": "tag_history",
            "tag": root.to_string(),
            "prerequisites": [
                {"kind": 4, "hash": landing.to_string()},
                {"kind": 22, "hash": apply.to_string()},
            ],
        })
    );
    assert_eq!(
        json["keys"][1],
        serde_json::json!({"kind": 6, "hash": chunk.to_string()})
    );
    assert_eq!(json["digest"], serde_json::json!(digest.to_string()));
    assert_eq!(json["records"], serde_json::json!(3));
    assert_eq!(
        serde_json::to_value(&records[1]).unwrap(),
        serde_json::json!({"dropped": {
            "key": {"kind": 6, "hash": chunk.to_string()},
            "encoding": 0,
            "original_len": 5,
            "apply": apply.to_string(),
            "residue": null,
        }})
    );
    assert_eq!(
        serde_json::to_value(StreamPurpose::TagHistory).unwrap(),
        "tag_history"
    );
}

#[test]
fn ha5_first_batch_need_not_contain_the_root() {
    // the root TAG and its landing come in a later file; this one holds two leaves
    let root = tag("v1", Hash([4; 32])).id();
    let leaves = [
        present(kinds::BLOCK, &block("b-00001", "one")),
        present(kinds::BLOCK, &block("b-00002", "two")),
    ];
    let first = batch(&header(root, vec![key(kinds::TAG, root)]), &leaves);
    let report = check(&first).unwrap();
    assert_eq!(
        report.keys,
        leaves.iter().map(StreamRecord::key).collect::<Vec<_>>()
    );
    assert_eq!(report.header.prerequisites, vec![key(kinds::TAG, root)]);
    // without the root among its prerequisites, the root is unlisted
    let wrong = batch(&header(root, vec![]), &leaves);
    assert_eq!(
        check(&wrong).unwrap_err(),
        fault(
            0,
            PRELUDE,
            "root tag is not in the batch or its prerequisites",
            None
        )
    );
}

#[test]
fn ha5_zero_object_batch_can_name_a_held_root() {
    let root = tag("v1", Hash([4; 32])).id();
    let h = header(root, vec![key(kinds::TAG, root)]);
    let bytes = batch(&h, &[]);
    let report = check(&bytes).unwrap();
    assert!(report.keys.is_empty());
    assert_eq!(report.records, 0);
    assert_eq!(report.bytes, bytes.len() as u64 - HEAD as u64 - 48);
    // a zero-object batch must still name its root
    let none = batch(&header(root, vec![]), &[]);
    assert_eq!(
        check(&none).unwrap_err(),
        fault(
            0,
            PRELUDE,
            "root tag is not in the batch or its prerequisites",
            None
        )
    );
}

#[test]
fn ha5_batches_have_independent_trailers() {
    // one upload as two files: the blocks and their snapshot, then the tag that needs a landing
    // the receiver holds; each file has its own prerequisites, trailer and digest
    let landing = Hash([4; 32]);
    let t = tag("v1", landing);
    let root = t.id();
    let a = block("b-00001", "one");
    let b = block("b-00002", "two");
    let snap = StateSnapshot {
        blocks: BTreeMap::from([
            (a.id.clone(), a.version_hash()),
            (b.id.clone(), b.version_hash()),
        ]),
    };
    let one_records = [
        present(kinds::BLOCK, &a),
        present(kinds::BLOCK, &b),
        present(kinds::SNAPSHOT, &snap),
    ];
    let one = batch(&header(root, vec![key(kinds::TAG, root)]), &one_records);
    let two_records = [present(kinds::TAG, &t)];
    let two = batch(
        &header(root, vec![key(kinds::LANDING, landing)]),
        &two_records,
    );
    let (r1, r2) = (check(&one).unwrap(), check(&two).unwrap());
    assert_eq!((r1.records, r2.records), (3, 1));
    assert_ne!(r1.digest, r2.digest);
    assert_ne!(r1.header.prerequisites, r2.header.prerequisites);
    assert_eq!((r1.header.tag, r2.header.tag), (root, root), "one root");
    // each trailer ends only its own file: the two files glued are no batch
    let mut glued = one.clone();
    glued.extend_from_slice(&two);
    let trailer_end = one.len() as u64;
    assert_eq!(
        refusal(check(&glued).unwrap_err()),
        bad(4 + 1, trailer_end, "trailing bytes")
    );
    // checked in either order, each passes alone
    assert_eq!(check(&two).unwrap(), r2);
    assert_eq!(check(&one).unwrap(), r1);
}

#[test]
fn ha5_batch_prerequisites_are_exact_and_local() {
    let a = block("b-00001", "one");
    let snap = StateSnapshot {
        blocks: BTreeMap::from([(a.id.clone(), a.version_hash())]),
    };
    let t = tag("v1", Hash([4; 32]));
    let root = t.id();
    let a_key = key(kinds::BLOCK, a.version_hash());
    let root_key = key(kinds::TAG, root);
    let s = present(kinds::SNAPSHOT, &snap);

    // the snapshot needs its block: absent from the batch, it must be a prerequisite
    let ok = batch(
        &header(root, vec![a_key, root_key]),
        std::slice::from_ref(&s),
    );
    assert_eq!(
        check(&ok).unwrap().header.prerequisites,
        vec![a_key, root_key]
    );
    // a needed key that is not listed: named at the snapshot that needs it
    let first_at = PRELUDE + HEAD as u64 + 57 + manifest_len() as u64 + 33;
    let missing = batch(&header(root, vec![root_key]), std::slice::from_ref(&s));
    assert_eq!(
        check(&missing).unwrap_err(),
        fault(
            1,
            first_at,
            &format!("unlisted dependency 1:{}", a.version_hash()),
            Some(s.key())
        )
    );
    // a listed key no record needs
    let extra = key(kinds::BLOCK, Hash([9; 32]));
    let mut more = vec![a_key, extra, root_key];
    more.sort();
    let unneeded = batch(&header(root, more), std::slice::from_ref(&s));
    assert_eq!(
        check(&unneeded).unwrap_err(),
        fault(
            0,
            PRELUDE,
            &format!("unneeded prerequisite 1:{}", "09".repeat(32)),
            None
        )
    );
    // a listed key the batch holds is not absent
    let held = batch(
        &header(root, vec![a_key, root_key]),
        &[present(kinds::BLOCK, &a), s.clone()],
    );
    assert_eq!(
        check(&held).unwrap_err(),
        fault(0, PRELUDE, &format!("unneeded prerequisite {a_key}"), None)
    );
    // the root in the batch is no prerequisite; the batch needs only what its records need
    let with_root = batch(
        &header(root, vec![key(kinds::LANDING, t.landing)]),
        &[present(kinds::TAG, &t)],
    );
    assert!(check(&with_root).is_ok());
    let root_listed = batch(
        &header(root, vec![key(kinds::LANDING, t.landing), root_key]),
        &[present(kinds::TAG, &t)],
    );
    assert_eq!(
        check(&root_listed).unwrap_err(),
        fault(0, PRELUDE, &format!("unneeded prerequisite 9:{root}"), None)
    );

    // a dependency later in the batch (or the order of another file) is a cycle here
    let b = present(kinds::BLOCK, &a);
    let later = batch(&header(root, vec![root_key]), &[s.clone(), b.clone()]);
    assert_eq!(
        refusal(check(&later).unwrap_err()),
        bad(1, first_at, "dependency cycle")
    );
    let earlier = batch(&header(root, vec![root_key]), &[b, s.clone()]);
    assert!(check(&earlier).is_ok());

    // a kind-None link resolves to the one batch or prerequisite key of its hash
    let target = a.version_hash();
    let answer = rdy_model::Answer {
        item: "P-x@1".into(),
        act: rdy_model::AnswerAct::Confirm,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![target],
        person: prov(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    let ans = present(kinds::ANSWER, &answer);
    let one = batch(
        &header(root, vec![a_key, root_key]),
        std::slice::from_ref(&ans),
    );
    assert!(check(&one).is_ok(), "resolves to the BLOCK prerequisite");
    let mut two_kinds = vec![a_key, key(kinds::LANDING, target), root_key];
    two_kinds.sort();
    let ambiguous = batch(&header(root, two_kinds), std::slice::from_ref(&ans));
    assert_eq!(
        refusal(check(&ambiguous).unwrap_err()),
        bad(1, first_at + 66, "ambiguous object kind")
    );
    let unresolved = batch(&header(root, vec![root_key]), std::slice::from_ref(&ans));
    assert_eq!(
        check(&unresolved).unwrap_err(),
        fault(
            1,
            first_at,
            &format!("unlisted dependency {target}"),
            Some(ans.key())
        ),
        "no batch check returns E_UNRESOLVED_REF"
    );

    // a dropped object needs its residue, and names its apply: an absent apply is a
    // prerequisite (fix round 1); an apply of the batch is a later proof, so it may follow
    let residue = Hash([21; 32]);
    let dropped = StreamRecord::Dropped {
        encoding: 0,
        key: a_key,
        original_len: 40,
        apply: Hash([22; 32]),
        residue: Some(residue),
    };
    let res_key = key(kinds::REDACTION_RESIDUE, residue);
    let apply_key = key(kinds::REDACTION_APPLY, Hash([22; 32]));
    let mut needs = vec![root_key, res_key, apply_key];
    needs.sort();
    let d = batch(&header(root, needs), std::slice::from_ref(&dropped));
    assert_eq!(
        check(&d).unwrap().header.prerequisites,
        vec![root_key, res_key, apply_key]
    );
    let mut no_apply = vec![root_key, res_key];
    no_apply.sort();
    let unlisted = batch(&header(root, no_apply), std::slice::from_ref(&dropped));
    // two listed keys: the first record is one key further
    let two_listed_at = first_at + 33;
    assert_eq!(
        check(&unlisted).unwrap_err(),
        fault(
            1,
            two_listed_at,
            &format!("unlisted dependency {apply_key}"),
            Some(a_key)
        ),
        "an absent apply that is no prerequisite"
    );
    let no_residue = batch(
        &header(root, vec![root_key, apply_key]),
        std::slice::from_ref(&dropped),
    );
    assert_eq!(
        check(&no_residue).unwrap_err(),
        fault(
            1,
            two_listed_at,
            &format!("unlisted dependency {res_key}"),
            Some(a_key)
        )
    );
    // the apply in the batch, after the placeholder: no prerequisite, no cycle
    let real_apply = present(
        kinds::REDACTION_APPLY,
        &rdy_model::redaction::RedactionApply {
            request: Hash([30; 32]),
            landing: Hash([31; 32]),
            report_hash: Hash([32; 32]),
        },
    );
    let StreamRecord::Dropped {
        key: k,
        original_len,
        residue: r,
        ..
    } = dropped.clone()
    else {
        unreachable!()
    };
    let proved = StreamRecord::Dropped {
        encoding: 0,
        key: k,
        original_len,
        apply: real_apply.key().hash,
        residue: r,
    };
    // the apply itself needs its request and its landing
    let mut needs = vec![
        root_key,
        res_key,
        key(kinds::REDACTION, Hash([30; 32])),
        key(kinds::LANDING, Hash([31; 32])),
    ];
    needs.sort();
    let later_proof = batch(
        &header(root, needs.clone()),
        &[proved.clone(), real_apply.clone()],
    );
    assert_eq!(check(&later_proof).unwrap().keys.len(), 2);
    let mut listed = needs.clone();
    listed.push(real_apply.key());
    listed.sort();
    let held_apply = batch(&header(root, listed), &[proved, real_apply.clone()]);
    assert_eq!(
        check(&held_apply).unwrap_err(),
        fault(
            0,
            PRELUDE,
            &format!("unneeded prerequisite {}", real_apply.key()),
            None
        ),
        "an apply the batch holds is no prerequisite"
    );

    // a repeated logical key, present or dropped
    let repeat = batch_unchecked(
        &header(root, vec![root_key]),
        &[raw(kinds::BLOB_CHUNK, b"c"), raw(kinds::BLOB_CHUNK, b"c")],
    );
    assert_eq!(
        refusal(check(&repeat).unwrap_err()),
        bad(2, first_at + 38, "object is repeated")
    );
}

/// `records` written as one batch by hand, without the writer's checks.
fn batch_unchecked(h: &StreamHeader, records: &[StreamRecord]) -> Vec<u8> {
    let mut bytes = opened(h);
    let mut chain = vec![];
    let hp = &bytes[PRELUDE as usize + HEAD..];
    link(&mut chain, 0xF0, hp);
    for r in records {
        let StreamRecord::Present { key, payload, .. } = r else {
            panic!("present records only")
        };
        bytes.extend(frame(key.kind, payload));
        link(&mut chain, key.kind, payload);
    }
    let mut trailer = (records.len() as u64).to_le_bytes().to_vec();
    trailer.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    bytes.extend(frame(0xFF, &trailer));
    bytes
}

#[test]
fn ha5_changed_hash_and_trailer_are_refused() {
    let t = tag("v1", Hash([4; 32]));
    let root = t.id();
    let a = block("b-00001", "one");
    let b = block("b-00002", "two");
    let h = header(root, vec![key(kinds::TAG, root)]);
    let bytes = batch(&h, &[present(kinds::BLOCK, &a), present(kinds::BLOCK, &b)]);
    let first_at = PRELUDE + HEAD as u64 + 57 + manifest_len() as u64 + 33;
    let second_at = first_at + HEAD as u64 + enc(&a).len() as u64;
    let trailer_at = second_at + HEAD as u64 + enc(&b).len() as u64;
    assert_eq!(trailer_at + HEAD as u64 + 48, bytes.len() as u64);

    // a changed payload byte: the second record, at its frame's first byte
    let mut changed = bytes.clone();
    changed[second_at as usize + HEAD + 3] ^= 1;
    let e = check(&changed).unwrap_err();
    assert_eq!(refusal(e.clone()), bad(2, second_at, "hash mismatch"));
    assert_eq!(e.object, Some(key(kinds::BLOCK, b.version_hash())));
    assert_eq!(
        e.fix,
        "request this stream again; run rdy fsck at its source"
    );
    // a changed header byte
    let mut header_byte = bytes.clone();
    header_byte[PRELUDE as usize + HEAD + 30] ^= 1;
    assert_eq!(
        refusal(check(&header_byte).unwrap_err()),
        bad(0, PRELUDE, "hash mismatch")
    );
    // a changed trailer byte breaks the trailer's own hash
    let mut trailer_byte = bytes.clone();
    *trailer_byte.last_mut().unwrap() ^= 1;
    assert_eq!(
        refusal(check(&trailer_byte).unwrap_err()),
        bad(3, trailer_at, "hash mismatch")
    );
    // a trailer that hashes but states other counts or another digest
    let truncated = |record: u64, at: u64| {
        (
            "E_STREAM_TRUNCATED",
            Some(record),
            at,
            "stream trailer is missing or does not match".to_string(),
        )
    };
    for field in [0usize, 8, 16] {
        let mut t = bytes[trailer_at as usize + HEAD..].to_vec();
        t[field] ^= 1;
        let mut other = bytes[..trailer_at as usize].to_vec();
        other.extend(frame(0xFF, &t));
        assert_eq!(
            refusal(check(&other).unwrap_err()),
            truncated(3, trailer_at),
            "field {field}"
        );
    }
    // a trailer of another length
    let mut long = bytes[..trailer_at as usize].to_vec();
    let mut t = bytes[trailer_at as usize + HEAD..].to_vec();
    t.push(0);
    long.extend(frame(0xFF, &t));
    assert_eq!(refusal(check(&long).unwrap_err()), truncated(3, trailer_at));
    // the file cut anywhere before its end
    for cut in [
        0usize,
        5,
        12,
        40,
        first_at as usize,
        second_at as usize + 10,
        trailer_at as usize,
        bytes.len() - 1,
    ] {
        let e = check(&bytes[..cut]).unwrap_err();
        assert_eq!(e.code, "E_STREAM_TRUNCATED", "cut at {cut}: {e}");
    }
    let e = check(&bytes[..second_at as usize + 10]).unwrap_err();
    assert_eq!((e.record, e.offset), (Some(2), second_at));
    // bytes after the trailer
    let mut after = bytes.clone();
    after.push(0);
    assert_eq!(
        refusal(check(&after).unwrap_err()),
        bad(4, bytes.len() as u64, "trailing bytes")
    );
    // frames of a kind no batch holds: local effects, the transition, the dump's own, unassigned
    for kind in [0u8, 8, 12, 14, 19, 23, 0xF0, 0xF1, 0xF2] {
        let mut k = bytes[..second_at as usize].to_vec();
        k.extend(frame(kind, &enc(&b)));
        k.extend_from_slice(&bytes[trailer_at as usize..]);
        assert_eq!(
            refusal(check(&k).unwrap_err()),
            bad(2, second_at, "unknown kind"),
            "kind {kind}"
        );
    }
    // a record that does not decode as its kind
    let mut garbled = bytes[..second_at as usize].to_vec();
    garbled.extend(frame(kinds::TAG, b"\xff\xff"));
    garbled.extend_from_slice(&bytes[trailer_at as usize..]);
    let e = check(&garbled).unwrap_err();
    assert_eq!(refusal(e.clone()).0, "E_BAD_RECORD");
    assert_eq!(
        e.message,
        format!("stream record 2 at byte {second_at}: record does not decode")
    );
    // a DROPPED record of another length or body
    let mut short = bytes[..second_at as usize].to_vec();
    short.extend(frame(0xF3, &[0u8; 101]));
    short.extend_from_slice(&bytes[trailer_at as usize..]);
    assert_eq!(
        refusal(check(&short).unwrap_err()),
        bad(2, second_at, "invalid dropped record")
    );
    let mut body = vec![kinds::TAG];
    body.extend_from_slice(&[0u8; 101]);
    let mut wrong_kind = bytes[..second_at as usize].to_vec();
    wrong_kind.extend(frame(0xF3, &body));
    wrong_kind.extend_from_slice(&bytes[trailer_at as usize..]);
    assert_eq!(
        refusal(check(&wrong_kind).unwrap_err()),
        bad(2, second_at, "invalid dropped record")
    );

    // the prelude and the header
    let mut magic = bytes.clone();
    magic[0] = b'X';
    assert_eq!(
        refusal(check(&magic).unwrap_err()),
        bad(0, 0, "invalid header")
    );
    let mut format = bytes.clone();
    format[8] = 3;
    assert_eq!(
        refusal(check(&format).unwrap_err()),
        bad(0, 8, "invalid header")
    );
    // stream format 1 is not read
    format[8] = 1;
    let e = check(&format).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_STORE_FORMAT",
            "sync batch format 1 is not supported",
            "write the stream with the pinned rdy build"
        )
    );
    let m = 4 + manifest_len();
    let header_with = |at: usize, value: &[u8]| {
        let mut hp = bytes[PRELUDE as usize + HEAD..first_at as usize].to_vec();
        hp[at..at + value.len()].copy_from_slice(value);
        let mut out = bytes[..PRELUDE as usize].to_vec();
        out.extend(frame(0xF0, &hp));
        out.extend_from_slice(&bytes[first_at as usize..]);
        out
    };
    let e = check(&header_with(m + 16, &[1])).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 1 does not match tag_history",
            "request a tag_history stream"
        )
    );
    // a manifest of another grammar: `{"manifest_version":2,...`
    let e = check(&header_with(4 + 20, b"2")).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_STORE_FORMAT",
            "store feature manifest is invalid: unsupported manifest version",
            "restore the store from a backup"
        )
    );
    // a prerequisite of a kind no batch holds
    assert_eq!(
        refusal(check(&header_with(m + 53, &[kinds::TRANSITION])).unwrap_err()),
        bad(0, PRELUDE, "invalid header")
    );
    // a first frame that is not the header
    let mut no_header = bytes[..PRELUDE as usize].to_vec();
    no_header.extend_from_slice(&bytes[first_at as usize..]);
    assert_eq!(
        refusal(check(&no_header).unwrap_err()),
        bad(0, PRELUDE, "invalid header")
    );

    // no read resumes after a failure
    let (mut r, _) = StreamReader::open(&changed[..]).unwrap();
    assert!(r.next().unwrap().is_some());
    let e1 = r.next().unwrap_err();
    assert_eq!(r.next().unwrap_err(), e1);
    assert_eq!(r.finish().unwrap_err(), e1);
}

/// A reader of a batch that is made as it is read: the prelude and a header, then `frames`
/// frames of `kind` with the empty payload, then a trailer that states them.
struct Many {
    bytes: Vec<u8>,
    at: usize,
    left: u64,
    frame: Vec<u8>,
    trailer: Option<Vec<u8>>,
}

impl Many {
    fn new(h: &StreamHeader, kind: u8, frames: u64) -> Many {
        let head = opened(h);
        let empty = frame(kind, b"");
        let mut chain = vec![];
        link(&mut chain, 0xF0, &head[PRELUDE as usize + HEAD..]);
        for _ in 0..frames {
            link(&mut chain, kind, b"");
        }
        let bytes_before = head.len() as u64 + frames * empty.len() as u64;
        let mut t = frames.to_le_bytes().to_vec();
        t.extend_from_slice(&bytes_before.to_le_bytes());
        t.extend_from_slice(&Hash::of(&chain).0);
        Many {
            bytes: head,
            at: 0,
            left: frames,
            frame: empty,
            trailer: Some(frame(0xFF, &t)),
        }
    }
}

impl Read for Many {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at == self.bytes.len() {
            if self.left > 0 {
                self.left -= 1;
                self.bytes = self.frame.clone();
            } else if let Some(t) = self.trailer.take() {
                self.bytes = t;
            } else {
                return Ok(0);
            }
            self.at = 0;
        }
        let n = buf.len().min(self.bytes.len() - self.at);
        buf[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// A reader of a batch whose object frames are the same payload of `kind` over and over:
/// `frames` frames of `payload`, then one of `last` bytes, then the trailer. Each frame is made
/// once and read again, so only the reader hashes.
struct Large {
    parts: Vec<(Vec<u8>, u64)>,
    part: usize,
    round: u64,
    at: usize,
}

impl Large {
    fn new(h: &StreamHeader, kind: u8, payload: Vec<u8>, frames: u64, last: Vec<u8>) -> Large {
        let head = opened(h);
        let mut chain = vec![];
        link(&mut chain, 0xF0, &head[PRELUDE as usize + HEAD..]);
        let one = frame(kind, &payload);
        let mut repeated = vec![];
        link(&mut repeated, kind, &payload);
        for _ in 0..frames {
            chain.extend_from_slice(&repeated);
        }
        link(&mut chain, kind, &last);
        let before = head.len() as u64 + frames * one.len() as u64 + (HEAD + last.len()) as u64;
        let mut t = (frames + 1).to_le_bytes().to_vec();
        t.extend_from_slice(&before.to_le_bytes());
        t.extend_from_slice(&Hash::of(&chain).0);
        Large {
            parts: vec![
                (head, 1),
                (one, frames),
                (frame(kind, &last), 1),
                (frame(0xFF, &t), 1),
            ],
            part: 0,
            round: 0,
            at: 0,
        }
    }
}

impl Read for Large {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let Some((bytes, rounds)) = self.parts.get(self.part) else {
            return Ok(0);
        };
        let n = out.len().min(bytes.len() - self.at);
        out[..n].copy_from_slice(&bytes[self.at..self.at + n]);
        self.at += n;
        if self.at == bytes.len() {
            self.at = 0;
            self.round += 1;
            if self.round == *rounds {
                self.round = 0;
                self.part += 1;
            }
        }
        Ok(n)
    }
}

fn limit(e: StreamError) -> (&'static str, String, String) {
    (e.code, e.message, e.fix)
}

fn exceeds(label: &str) -> (&'static str, String, String) {
    (
        "E_LIMIT",
        format!("stream exceeds {label}"),
        "reduce the stream to the stated limit".to_string(),
    )
}

/// Reads `r` to its end through the reader alone (no typed decode, no key rules).
fn read_all(r: impl Read) -> Result<u64, StreamError> {
    let (mut reader, _) = StreamReader::open(r)?;
    let mut n = 0;
    while reader.next()?.is_some() {
        n += 1;
    }
    reader.finish()?;
    Ok(n)
}

#[test]
fn ha5_lengths_counts_edges_and_depth_are_bounded() {
    let root = tag("v1", Hash([4; 32])).id();
    let root_key = key(kinds::TAG, root);
    let only_root = header(root, vec![root_key]);

    // prerequisites: 100000, then 100001
    let keys = |n: usize| -> Vec<ObjectKey> {
        let mut v: Vec<ObjectKey> = (0..n as u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[..4].copy_from_slice(&i.to_be_bytes());
                h[31] = 1;
                key(kinds::BLOCK, Hash(h))
            })
            .collect();
        v.push(root_key);
        v.sort();
        v
    };
    let max = keys(99_999);
    assert_eq!(max.len(), 100_000);
    let h = header(root, max.clone());
    let bytes = batch(&h, &[]);
    let cap = 3_300_057 + manifest_len();
    assert_eq!(
        bytes.len(),
        12 + HEAD + cap + HEAD + 48,
        "the header cap of 100000 keys"
    );
    let (_, read) = StreamReader::open(&bytes[..]).unwrap();
    assert_eq!(read.prerequisites.len(), 100_000);
    let over = header(root, keys(100_000));
    let Err(e) = StreamWriter::new(Vec::new(), &over) else {
        panic!("the writer refuses 100001 prerequisites")
    };
    assert_eq!(limit(e), exceeds("100000 prerequisites"));
    // a header that says 100001 keys, and one longer than the cap
    let mut hp = bytes[PRELUDE as usize + HEAD..PRELUDE as usize + HEAD + cap].to_vec();
    let m = 4 + manifest_len();
    hp[m + 49..m + 53].copy_from_slice(&100_001u32.to_le_bytes());
    let mut said = bytes[..PRELUDE as usize].to_vec();
    said.extend(frame(0xF0, &hp));
    let Err(e) = StreamReader::open(&said[..]) else {
        panic!("the reader refuses 100001 prerequisites")
    };
    assert_eq!(limit(e), exceeds("100000 prerequisites"));
    let mut long = bytes[..PRELUDE as usize].to_vec();
    long.extend(frame_head(0xF0, 3_365_594));
    let Err(e) = StreamReader::open(&long[..]) else {
        panic!("the reader refuses a longer header")
    };
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_LIMIT", "stream exceeds 3365593 header bytes")
    );

    // record payloads: a typed record's 16777216 bytes, a chunk's 1048576, claimed by the frame
    // header alone (nothing is read or reserved for them)
    let start = opened(&only_root);
    for (kind, len, label) in [
        (kinds::BLOCK, 16_777_217u32, "16777216 record bytes"),
        (kinds::BLOB_CHUNK, 1_048_577, "1048576 chunk bytes"),
    ] {
        let mut b = start.clone();
        b.extend(frame_head(kind, len));
        let e = read_all(&b[..]).unwrap_err();
        assert_eq!(limit(e.clone()), exceeds(label));
        assert_eq!((e.record, e.offset), (Some(1), start.len() as u64));
    }
    let chunk = vec![1u8; 1_048_576];
    let at_bound = batch(&only_root, &[raw(kinds::BLOB_CHUNK, &chunk)]);
    assert_eq!(check(&at_bound).unwrap().records, 1);

    // pending dropped bytes: 16 chunks of 64 MiB claim 1073741824 bytes; a 17th claims more
    let drops = |n: u8| -> Vec<StreamRecord> {
        (0..n)
            .map(|i| StreamRecord::Dropped {
                encoding: 0,
                key: key(kinds::BLOB_CHUNK, Hash([i + 1; 32])),
                original_len: 64 << 20,
                apply: Hash([200; 32]),
                residue: None,
            })
            .collect()
    };
    // the drops name one absent apply, a prerequisite (fix round 1)
    let with_apply = header(
        root,
        vec![root_key, key(kinds::REDACTION_APPLY, Hash([200; 32]))],
    );
    let sixteen = batch(&with_apply, &drops(16));
    assert_eq!(check(&sixteen).unwrap().records, 16);
    let mut w = StreamWriter::new(Vec::new(), &with_apply).unwrap();
    let seventeen = drops(17);
    for r in &seventeen[..16] {
        w.record(r).unwrap();
    }
    assert_eq!(
        limit(w.record(&seventeen[16]).unwrap_err()),
        exceeds("1073741824 pending dropped bytes")
    );
    let mut b = sixteen[..sixteen.len() - HEAD - 48].to_vec();
    let mut body = vec![kinds::BLOB_CHUNK, 0, 0];
    body.extend_from_slice(&[17u8; 32]);
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&[200u8; 32]);
    body.push(0);
    body.extend_from_slice(&[0u8; 32]);
    b.extend(frame(0xF3, &body));
    let e = read_all(&b[..]).unwrap_err();
    assert_eq!(
        limit(e.clone()),
        exceeds("1073741824 pending dropped bytes")
    );
    assert_eq!(e.record, Some(17));
    // one dropped frame claims at most 64 MiB, never reserved
    let mut over_claim = drops(1);
    if let StreamRecord::Dropped { original_len, .. } = &mut over_claim[0] {
        *original_len = (64 << 20) + 1;
    }
    let mut w = StreamWriter::new(Vec::new(), &only_root).unwrap();
    assert_eq!(
        refusal(w.record(&over_claim[0]).unwrap_err()),
        bad(1, start.len() as u64, "invalid dropped record")
    );

    // object records: 1000000, then 1000001 (the reader's rule; the batch rules would refuse
    // the repeats)
    assert_eq!(MAX_RECORDS, 1_000_000);
    assert_eq!(
        read_all(Many::new(&only_root, kinds::BLOB_CHUNK, 1_000_000)).unwrap(),
        1_000_000
    );
    let e = read_all(Many::new(&only_root, kinds::BLOB_CHUNK, 1_000_001)).unwrap_err();
    assert_eq!(limit(e.clone()), exceeds("1000000 object records"));
    assert_eq!(e.record, Some(1_000_001));

    // total bytes: in ha5_total_bytes_are_bounded_at_n and _at_n_plus_one (each reads 2 GiB)

    // graph edges: 1000000, then 1000001 (ten snapshots of 99999 blocks and one of 10, then 11)
    let blocks: Vec<Hash> = (0..99_999u32)
        .map(|i| {
            let mut h = [0u8; 32];
            h[..4].copy_from_slice(&i.to_be_bytes());
            h[31] = 1;
            Hash(h)
        })
        .collect();
    let snapshot = |offset: usize, n: usize| -> StreamRecord {
        // distinct ids per snapshot keep each record distinct
        let prefix = [
            "sa", "sb", "sc", "sd", "se", "sf", "sg", "sh", "si", "sj", "sk",
        ][offset];
        present(
            kinds::SNAPSHOT,
            &StateSnapshot {
                blocks: blocks[..n]
                    .iter()
                    .enumerate()
                    .map(|(i, h)| (BlockId::parse(&format!("{prefix}-{i:05}")).unwrap(), *h))
                    .collect(),
            },
        )
    };
    let mut prereqs: Vec<ObjectKey> = blocks.iter().map(|h| key(kinds::BLOCK, *h)).collect();
    prereqs.push(root_key);
    prereqs.sort();
    let edges_header = header(root, prereqs);
    let mut records: Vec<StreamRecord> = (0..10).map(|i| snapshot(i, 99_999)).collect();
    records.push(snapshot(10, 10));
    let report = check(&batch(&edges_header, &records)).unwrap();
    assert_eq!(report.records, 11);
    records[10] = snapshot(10, 11);
    let e = check(&batch(&edges_header, &records)).unwrap_err();
    assert_eq!(limit(e.clone()), exceeds("1000000 graph edges"));
    assert_eq!(e.record, Some(11));

    // history depth: a chain of 100000 records, then 100001
    let chain = |n: u64| -> Vec<StreamRecord> {
        let mut out = Vec::with_capacity(n as usize);
        let mut prev = None;
        for i in 0..n {
            let r = present(kinds::INTENT, &intent(prev, i));
            prev = Some(r.key().hash);
            out.push(r);
        }
        out
    };
    let deep = chain(100_000);
    assert_eq!(check(&batch(&only_root, &deep)).unwrap().records, 100_000);
    let deeper = chain(100_001);
    let e = check(&batch(&only_root, &deeper)).unwrap_err();
    assert_eq!(limit(e.clone()), exceeds("100000 history depth"));
    assert_eq!(e.record, Some(100_001));

    // the labels of the upload-wide bounds
    assert_eq!(
        (MAX_BATCH_FILES, LIMIT_BATCH_FILES),
        (1024, "1024 batch files")
    );
    assert_eq!(LIMIT_DIGEST_BYTES, "41000041 digest bytes");
}

/// The batch of `127` frames of 16 MiB and one more that ends its trailer at exactly
/// [`MAX_STREAM_BYTES`] plus `over` bytes (the same payload over and over; the reader's rule).
fn two_gib(over: usize) -> Large {
    let root = tag("v1", Hash([4; 32])).id();
    let only_root = header(root, vec![key(kinds::TAG, root)]);
    let head_len = opened(&only_root).len() as u64;
    let payload = vec![3u8; 16 << 20];
    let frames = 127u64;
    let used =
        head_len + frames * (HEAD as u64 + payload.len() as u64) + HEAD as u64 + HEAD as u64 + 48;
    let last = (MAX_STREAM_BYTES - used) as usize + over;
    Large::new(
        &only_root,
        kinds::BLOB_MANIFEST,
        payload,
        frames,
        vec![5u8; last],
    )
}

#[test]
fn ha5_total_bytes_are_bounded_at_n() {
    assert_eq!(read_all(two_gib(0)).unwrap(), 128);
}

#[test]
fn ha5_total_bytes_are_bounded_at_n_plus_one() {
    let e = read_all(two_gib(1)).unwrap_err();
    assert_eq!(limit(e.clone()), exceeds("2147483648 total bytes"));
    assert_eq!(e.record, Some(129), "the trailer would pass the bound");
}

/// The exact E_BAD_RECORD error of record `record` at byte `offset` for `reason`, naming
/// `object`.
fn fault(record: u64, offset: u64, reason: &str, object: Option<ObjectKey>) -> StreamError {
    StreamError {
        code: "E_BAD_RECORD",
        message: format!("stream record {record} at byte {offset}: {reason}"),
        fix: "request this stream again; run rdy fsck at its source".to_string(),
        record: Some(record),
        offset,
        object,
    }
}

/// The frame offset of each of `records` in a batch whose header lists `prerequisites` keys:
/// the first at 12 + 37 + 57 + the manifest + 33k, each next one frame header and payload
/// further.
fn offsets(prerequisites: usize, records: &[StreamRecord]) -> Vec<u64> {
    let mut at = PRELUDE + HEAD as u64 + 57 + manifest_len() as u64 + 33 * prerequisites as u64;
    let mut out = vec![];
    for r in records {
        out.push(at);
        let len = match r {
            StreamRecord::Present { payload, .. } => payload.len() as u64,
            StreamRecord::Dropped { .. } => 104,
        };
        at += HEAD as u64 + len;
    }
    out
}

/// An answer that confirms with `targets` and nothing else.
fn answer(targets: Vec<Hash>) -> rdy_model::Answer {
    rdy_model::Answer {
        item: "P-x@1".into(),
        act: rdy_model::AnswerAct::Confirm,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: prov(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    }
}

fn sorted(mut keys: Vec<ObjectKey>) -> Vec<ObjectKey> {
    keys.sort();
    keys
}

#[test]
fn plb2_dependency_errors_keep_the_source_position() {
    let a = block("b-00001", "one");
    let b = block("b-00002", "two");
    let root = tag("v1", Hash([4; 32])).id();
    let root_key = key(kinds::TAG, root);
    let leaf = present(kinds::BLOCK, &b);
    let refused = |prerequisites: Vec<ObjectKey>, records: &[StreamRecord]| {
        check(&batch(&header(root, prerequisites), records)).unwrap_err()
    };

    // a typed edge: the snapshot needs block a, which is neither in the batch nor listed
    let snap = StateSnapshot {
        blocks: BTreeMap::from([
            (a.id.clone(), a.version_hash()),
            (b.id.clone(), b.version_hash()),
        ]),
    };
    let s = present(kinds::SNAPSHOT, &snap);
    let records = [leaf.clone(), s.clone()];
    let at = offsets(1, &records);
    assert_eq!(
        refused(vec![root_key], &records),
        fault(
            2,
            at[1],
            &format!("unlisted dependency 1:{}", a.version_hash()),
            Some(s.key())
        )
    );

    // an untyped edge: the answer's target matches no key, so it prints no kind
    let ans = present(kinds::ANSWER, &answer(vec![Hash([0x5a; 32])]));
    let records = [leaf.clone(), ans.clone()];
    let at = offsets(1, &records);
    assert_eq!(
        refused(vec![root_key], &records),
        fault(
            2,
            at[1],
            &format!("unlisted dependency {}", "5a".repeat(32)),
            Some(ans.key())
        )
    );

    // a dropped record's residue is an ordinary edge of the dropped record
    let a_key = key(kinds::BLOCK, a.version_hash());
    let apply_key = key(kinds::REDACTION_APPLY, Hash([22; 32]));
    let residue = StreamRecord::Dropped {
        encoding: 0,
        key: a_key,
        original_len: 40,
        apply: Hash([22; 32]),
        residue: Some(Hash([21; 32])),
    };
    let records = [leaf.clone(), residue];
    let at = offsets(2, &records);
    assert_eq!(
        refused(sorted(vec![root_key, apply_key]), &records),
        fault(
            2,
            at[1],
            &format!("unlisted dependency 21:{}", "15".repeat(32)),
            Some(a_key)
        )
    );

    // a dropped record's apply proof: two dropped records share it, the first one is named
    let dropped = |chunk: u8| StreamRecord::Dropped {
        encoding: 0,
        key: key(kinds::BLOB_CHUNK, Hash([chunk; 32])),
        original_len: 5,
        apply: Hash([22; 32]),
        residue: None,
    };
    let records = [leaf.clone(), dropped(6), dropped(7)];
    let at = offsets(1, &records);
    assert_eq!(
        refused(vec![root_key], &records),
        fault(
            2,
            at[1],
            &format!("unlisted dependency 22:{}", "16".repeat(32)),
            Some(key(kinds::BLOB_CHUNK, Hash([6; 32])))
        )
    );

    // an ordinary fault is reported before a proof fault, even at a later record
    let records = [dropped(6), ans.clone()];
    let at = offsets(1, &records);
    assert_eq!(
        refused(vec![root_key], &records),
        fault(
            2,
            at[1],
            &format!("unlisted dependency {}", "5a".repeat(32)),
            Some(ans.key())
        )
    );
}

#[test]
fn plb2_header_dependency_errors_are_specific_and_ordered() {
    let a = block("b-00001", "one");
    let a_key = key(kinds::BLOCK, a.version_hash());
    let root = tag("v1", Hash([4; 32])).id();
    let root_key = key(kinds::TAG, root);
    let leaf = present(kinds::BLOCK, &block("b-00002", "two"));
    let snap = present(
        kinds::SNAPSHOT,
        &StateSnapshot {
            blocks: BTreeMap::from([(a.id.clone(), a.version_hash())]),
        },
    );
    let refused = |prerequisites: Vec<ObjectKey>, records: &[StreamRecord]| {
        check(&batch(&header(root, prerequisites), records)).unwrap_err()
    };
    let no_root = "root tag is not in the batch or its prerequisites";

    // the root is neither in the batch nor listed, with records or without
    assert_eq!(
        refused(vec![], std::slice::from_ref(&leaf)),
        fault(0, PRELUDE, no_root, None)
    );
    assert_eq!(refused(vec![], &[]), fault(0, PRELUDE, no_root, None));

    // two unneeded prerequisites, after a needed one: the smallest unneeded key is named
    let landing = key(kinds::LANDING, Hash([0x30; 32]));
    let snapshot = key(kinds::SNAPSHOT, Hash([0x01; 32]));
    let listed = sorted(vec![snapshot, root_key, landing, a_key]);
    assert_eq!(listed[0], a_key, "a needed key comes first");
    assert_eq!(
        refused(listed, std::slice::from_ref(&snap)),
        fault(
            0,
            PRELUDE,
            &format!("unneeded prerequisite 4:{}", "30".repeat(32)),
            None
        )
    );

    // the order of the checks: edges, then the root, then the unneeded prerequisites
    assert_eq!(
        refused(vec![landing], std::slice::from_ref(&leaf)),
        fault(0, PRELUDE, no_root, None),
        "an absent root before an unneeded prerequisite"
    );
    let at = offsets(1, std::slice::from_ref(&snap));
    assert_eq!(
        refused(vec![landing], std::slice::from_ref(&snap)),
        fault(
            1,
            at[0],
            &format!("unlisted dependency 1:{}", a.version_hash()),
            Some(snap.key())
        ),
        "an unlisted dependency before an absent root"
    );

    // a header's own order and shape faults are still an invalid header
    let two = sorted(vec![a_key, root_key]);
    let bytes = batch(&header(root, two.clone()), std::slice::from_ref(&snap));
    let records_at = PRELUDE as usize + HEAD + 57 + manifest_len() + 2 * 33;
    let reheader = |count: u32, keys: &[ObjectKey]| {
        let mut hp =
            bytes[PRELUDE as usize + HEAD..PRELUDE as usize + HEAD + 53 + manifest_len()].to_vec();
        hp.extend_from_slice(&count.to_le_bytes());
        for k in keys {
            hp.push(k.kind);
            hp.extend_from_slice(&k.hash.0);
        }
        let mut out = bytes[..PRELUDE as usize].to_vec();
        out.extend(frame(0xF0, &hp));
        out.extend_from_slice(&bytes[records_at..]);
        out
    };
    assert_eq!(
        reheader(2, &two),
        bytes,
        "the header as the writer wrote it"
    );
    let invalid = fault(0, PRELUDE, "invalid header", None);
    assert_eq!(check(&reheader(2, &[two[1], two[0]])).unwrap_err(), invalid);
    assert_eq!(check(&reheader(2, &[two[0], two[0]])).unwrap_err(), invalid);
    assert_eq!(check(&reheader(3, &two)).unwrap_err(), invalid);
}
