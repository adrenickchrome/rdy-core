//! Versioned objects (D-WWAK7: "Each object records its encoding version"; D-F0904; D-69AY3): a
//! stored frame of encoding 0 keeps its bytes, place and hash through split, dump, load and
//! reindex; a versioned frame's raw kind has the high bit, its payload starts with the version,
//! and its hash covers the version; a sync stream keys every record by its logical kind.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::encoding::encoded_hash;
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::bounded::RecordError;
use rdy_store::codec::{self, ObjectBytes};
use rdy_store::dump::*;
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader, batch_end};
use rdy_store::format::{DumpStore, REF_STORE_FEATURES, StoreFeatures, baseline_features};
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::Reason;
use rdy_store::sync_stream::{StreamHeader, StreamReader, StreamRecord, StreamWriter, check_batch};
use rdy_store::{Store, StoreError};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::path::Path;

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

fn batches_of(s: &Store) -> Vec<DumpBatch> {
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

fn manifest_of(s: &Store) -> StoreFeatures {
    StoreFeatures::parse(&s.get_ref(REF_STORE_FEATURES).unwrap().unwrap()).unwrap()
}

/// A full dump of `batches`, of a store of `features`.
fn write_dump(features: &StoreFeatures, batches: &[DumpBatch]) -> Vec<u8> {
    let (log_end, marker) = match batches.last() {
        Some(b) => (batch_end(b).unwrap(), BatchMarker::of(b)),
        None => (BatchMarker::ZERO.start, BatchMarker::ZERO),
    };
    let header = DumpHeader {
        store: DumpStore::Features(features.clone()),
        log_end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(RepoId([7; 16]), marker)),
    };
    let mut tally = RemovalTally::default();
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in batches {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    w.finish(tally.facts().unwrap()).unwrap().0
}

fn read_dump(bytes: &[u8]) -> Result<(u32, DumpHeader, Vec<DumpBatch>), StoreError> {
    let (mut r, format, h) = DumpReader::open(bytes)?;
    let mut out = vec![];
    while let Some(item) = r.next()? {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    Ok((format, h, out))
}

fn segment(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join("segments/seg-000000.log")).unwrap()
}

#[test]
fn ok1_frame_versions_preserve_raw_zero_bytes() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let mut s = Store::create(&src).unwrap();
    let a = block("b-00001", "alpha");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    let blob = s.put_blob(b"a blob of encoding zero").unwrap();
    let features = manifest_of(&s);
    assert_eq!(features, baseline_features());
    let batches = batches_of(&s);
    drop(s);
    let bytes = segment(&src);

    // every stored frame is of encoding 0: unmarked, its payload its body, its hash the body's
    let mut at = 0usize;
    for b in &batches {
        for f in &b.frames {
            let Frame::Present {
                kind,
                hash,
                payload,
            } = f
            else {
                panic!("no frame is dropped here");
            };
            assert_eq!(kind & codec::VERSIONED, 0);
            assert_eq!(
                codec::split(*kind, payload).unwrap(),
                ObjectBytes {
                    kind: *kind,
                    version: 0,
                    body: payload
                }
            );
            assert_eq!(*hash, encoded_hash(0, payload));
            assert_eq!(codec::latest(*kind), 0, "every kind's latest encoding is 0");
            // the frame's exact bytes at its place: header, then the payload
            let frame_at = bytes[at..]
                .windows(32)
                .position(|w| w == hash.0)
                .map(|p| at + p - 5)
                .expect("the frame is in the segment");
            assert_eq!(bytes[frame_at + 4], *kind);
            assert_eq!(
                &bytes[frame_at + 37..frame_at + 37 + payload.len()],
                &payload[..]
            );
            at = frame_at + 37 + payload.len();
        }
    }

    // dump 4 carries the frames byte for byte
    let dump = write_dump(&features, &batches);
    let (format, header, back) = read_dump(&dump).unwrap();
    assert_eq!(format, DUMP_FORMAT);
    assert_eq!(DUMP_FORMAT, 4);
    assert_eq!(header.store, DumpStore::Features(features.clone()));
    assert_eq!(back, batches);

    // a load writes the same log, byte for byte, with no batch of its own
    let to = base.path().join("to");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in back.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    assert!(
        !i.upgrade(&header.store).unwrap(),
        "a dump-4 load appends nothing"
    );
    i.finish().unwrap();
    assert_eq!(segment(&to), bytes);
    assert_eq!(
        std::fs::read(to.join("FORMAT")).unwrap(),
        std::fs::read(src.join("FORMAT")).unwrap()
    );
    // and its index, built and rebuilt from the log, reads every object
    let mut t = Store::open(&to).unwrap();
    t.reindex().unwrap();
    assert_eq!(t.get_blob(&blob).unwrap(), b"a blob of encoding zero");
    assert_eq!(t.get_block(&a.version_hash()).unwrap(), a);
    assert_eq!(manifest_of(&t), features);
    assert_eq!(segment(&to), bytes, "nothing was rewritten");
}

#[test]
fn ok1_version_prefix_is_hashed() {
    let tag = Tag {
        name: "v1".into(),
        landing: Hash([4; 32]),
        message: None,
        tagged_by: Provenance {
            actor: "human:a".into(),
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
    };
    let body = postcard::to_allocvec(&tag).unwrap();
    let _v1 = codec::test_encoding(kinds::TAG, 1);
    assert_eq!(codec::latest(kinds::TAG), 1);
    let (raw, payload) = codec::frame(kinds::TAG, 1, body.clone()).unwrap();
    assert_eq!(raw, kinds::TAG | 0x80);
    assert_eq!(&payload[..2], &[1, 0]);
    assert_eq!(&payload[2..], &body[..]);
    assert_eq!(Hash::of(&payload), encoded_hash(1, &body));
    assert_ne!(Hash::of(&payload), Hash::of(&body), "the version is hashed");
    assert_eq!(
        codec::split(raw, &payload).unwrap(),
        ObjectBytes {
            kind: kinds::TAG,
            version: 1,
            body: &body
        }
    );
    assert_eq!(
        codec::decode_record::<Tag>(raw, &payload).unwrap(),
        tag,
        "a versioned body decodes into the current type"
    );
    // the writer's encoding is the kind's latest
    let e = codec::encode(kinds::TAG, &tag).unwrap();
    assert_eq!((e.kind, e.version), (kinds::TAG, 1));
    assert_eq!(e.payload, payload);
    // another version prefix is another hash, and an unknown version
    let mut other = payload.clone();
    other[0] = 2;
    assert_ne!(Hash::of(&other), Hash::of(&payload));
    assert_eq!(codec::split(raw, &other), Err(RecordError::UnknownVersion));
    // a forged header/hash pair, the body's hash on the versioned payload, fails verification
    let forged = DumpBatch {
        start: BatchMarker::ZERO.start,
        digest: kinds::batch_digest([Hash::of(&body)].iter()),
        frames: vec![Frame::Present {
            kind: raw,
            hash: Hash::of(&body),
            payload: payload.clone(),
        }],
    };
    let header = DumpHeader {
        store: DumpStore::Features(baseline_features()),
        log_end: batch_end(&forged).unwrap(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(
            RepoId([7; 16]),
            BatchMarker::of(&forged),
        )),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    match w.batch(&forged) {
        Err(StoreError::Refused(m)) => assert_eq!(m, "a record's hash does not match its bytes"),
        other => panic!("{other:?}"),
    }
    let key = ObjectKey {
        kind: kinds::TAG,
        hash: Hash::of(&body),
    };
    let mut sw = StreamWriter::new(
        Vec::new(),
        &StreamHeader::tag_history(baseline_features(), RepoId([7; 16]), key.hash, vec![]),
    )
    .unwrap();
    let e = sw
        .record(&StreamRecord::Present {
            key,
            encoding: 1,
            payload,
        })
        .unwrap_err();
    assert_eq!(
        e.message,
        format!("stream record 1 at byte {}: hash mismatch", e.offset)
    );
}

#[test]
fn ok1_stream_two_preserves_logical_keys() {
    let tag = Tag {
        name: "v1".into(),
        landing: Hash([4; 32]),
        message: None,
        tagged_by: Provenance {
            actor: "human:a".into(),
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
    };
    let landing = ObjectKey {
        kind: kinds::LANDING,
        hash: Hash([4; 32]),
    };
    let body = postcard::to_allocvec(&tag).unwrap();
    let stream = |encoding: u16| -> (Vec<u8>, ObjectKey) {
        let (_, payload) = codec::frame(kinds::TAG, encoding, body.clone()).unwrap();
        let key = ObjectKey {
            kind: kinds::TAG,
            hash: Hash::of(&payload),
        };
        let mut w = StreamWriter::new(
            Vec::new(),
            &StreamHeader::tag_history(
                baseline_features(),
                RepoId([7; 16]),
                key.hash,
                vec![landing],
            ),
        )
        .unwrap();
        w.record(&StreamRecord::Present {
            key,
            encoding,
            payload,
        })
        .unwrap();
        (w.finish().unwrap().0, key)
    };
    let (zero, zero_key) = stream(0);
    let report = check_batch(&zero[..]).unwrap();
    assert_eq!(report.keys, [zero_key]);
    assert_eq!(report.header.stream_format, 2);
    assert_eq!(report.header.features, baseline_features());
    {
        let _v1 = codec::test_encoding(kinds::TAG, 1);
        let (one, one_key) = stream(1);
        // the frame on the wire keeps its raw kind; the record and its key are logical
        let header_len = u32::from_le_bytes(one[12..16].try_into().unwrap()) as usize;
        let first = 12 + 37 + header_len;
        assert_eq!(
            one[first + 4],
            kinds::TAG | 0x80,
            "the raw kind on the wire"
        );
        let report = check_batch(&one[..]).unwrap();
        assert_eq!(report.keys, [one_key]);
        assert_eq!(one_key.kind, kinds::TAG);
        assert_ne!(one_key.hash, zero_key.hash);
        assert_eq!(
            report.header.prerequisites,
            [landing],
            "the prerequisites are logical keys, whatever the encoding"
        );
        let (mut r, _) = StreamReader::open(&one[..]).unwrap();
        match r.next().unwrap().unwrap() {
            StreamRecord::Present {
                key,
                encoding,
                payload,
            } => {
                assert_eq!((key, encoding), (one_key, 1));
                assert_eq!(&payload[..2], &[1, 0]);
            }
            other => panic!("{other:?}"),
        }
        drop(_v1);
        // with no encoding 1 registered, the versioned record is refused
        let e = check_batch(&one[..]).unwrap_err();
        assert_eq!(e.code, "E_BAD_RECORD");
        assert!(
            e.message.ends_with(Reason::UnknownEncodingVersion.as_str()),
            "{}",
            e.message
        );
        assert_eq!(e.record, Some(1));
    }
}

#[test]
fn ok1_dropped_project_frames_keep_their_encoding() {
    let base = tempfile::tempdir().unwrap();
    let path = base.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "canary alpha secret");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    drop(s);
    let spec = RequestSpec::new(
        "work",
        vec![RedactTarget::Version {
            id: a.id.clone(),
            version: a.version_hash(),
        }],
    );
    authorized_batch_fixture(
        &path,
        &[spec],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let s = Store::open(&path).unwrap();
    let features = manifest_of(&s);
    let batches = batches_of(&s);
    drop(s);
    let dropped: Vec<&Frame> = batches
        .iter()
        .flat_map(|b| &b.frames)
        .filter(|f| matches!(f, Frame::Dropped { .. }))
        .collect();
    assert!(!dropped.is_empty());
    let dump = write_dump(&features, &batches);
    // each DROPPED record: logical kind, encoding 0, then the format-3 proof order
    let (_, _, back) = read_dump(&dump).unwrap();
    assert_eq!(back, batches, "kind, length and proof survive");
    for f in &dropped {
        let Frame::Dropped {
            kind,
            hash,
            len,
            apply,
            residue,
        } = f
        else {
            unreachable!()
        };
        let mut want = vec![*kind, 0, 0];
        want.extend_from_slice(&hash.0);
        want.extend_from_slice(&len.to_le_bytes());
        want.extend_from_slice(&apply.0);
        want.push(1);
        want.extend_from_slice(&residue.unwrap().0);
        assert_eq!(want.len(), ENCODED_DROPPED_LEN);
        let at = dump
            .windows(want.len())
            .position(|w| w == want)
            .expect("the DROPPED record");
        assert_eq!(dump[at - 33], rec::DROPPED);
        // a versioned removal has no residue adapter: refused
        let mut forged = dump.clone();
        forged[at + 1] = 1;
        let h = Hash::of(&forged[at..at + ENCODED_DROPPED_LEN]);
        forged[at - 32..at].copy_from_slice(&h.0);
        match read_dump(&forged) {
            Err(StoreError::Dump { what, .. }) => assert_eq!(what, "unknown encoding version"),
            other => panic!("{other:?}"),
        }
    }
}

fn who(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: Some("r".into()),
        role,
        at_ms: 7,
    }
}

/// Every writer asks the codec for its kind's latest encoding: with encoding 1 registered for
/// each kind whose store identity is its stored frame's hash, every object a writer authors is
/// a versioned frame of version 1, the hash each writer returns and each ref names is that
/// frame's, and the store reads, reindexes and reopens as before. BLOCK and CHANGE keep their
/// model identities (`Block::version_hash`, `Change::id`) until their encodings move.
#[test]
fn ok1_every_writer_writes_its_kinds_latest_encoding() {
    let versioned = [
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        kinds::BLOB_CHUNK,
        kinds::BLOB_MANIFEST,
        kinds::TAG,
        kinds::ANSWER,
        kinds::SIGNATURE,
        kinds::VIEW_OP,
        kinds::INTENT,
        kinds::ALT_SET,
        kinds::REVIEW,
    ];
    let _v1: Vec<_> = versioned
        .iter()
        .map(|k| codec::test_encoding(*k, 1))
        .collect();
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let created = batches_of(&s).len();
    let side = s
        .view_create("side", None, None, false, &who("agent:w", Role::Writer))
        .unwrap();
    let cid = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block("b-00001", "alpha"),
                }],
            ),
        )
        .unwrap();
    let proposal = s
        .propose_changes(
            "work",
            "first",
            Some("the first version"),
            who("agent:w", Role::Writer),
            vec![cid],
            None,
        )
        .unwrap();
    let landing = s
        .land(&proposal, who("human:alice", Role::Approver))
        .unwrap();
    let (tag, answer) = s
        .put_tag_answered(
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: who("human:alice", Role::Approver),
            },
            Answer {
                item: "tag:v1".into(),
                act: AnswerAct::Tag,
                option: None,
                label: None,
                options: vec![],
                card_hash: None,
                targets: vec![],
                person: who("human:alice", Role::Approver),
                relayed_by: None,
                relay_evidence: None,
                evidence_kind: None,
                note: None,
            },
        )
        .unwrap();
    let signature = s
        .put_signature(&rdy_model::signature::Signature {
            subject: landing,
            namespace: "rdy-approval-v1".into(),
            statement: b"statement".to_vec(),
            sig: vec![1],
            signed_by: who("human:alice", Role::Approver),
        })
        .unwrap();
    let blob = s.put_blob(b"a blob").unwrap();
    let intent = s
        .put_intent(&Intent {
            stable: None,
            prev: None,
            actor: "agent:a".into(),
            run: Some("r".into()),
            view: "work".into(),
            area: None,
            blocks: vec![],
            angle: "an angle".into(),
            expires_at_ms: 10_000,
            status: IntentStatus::Open,
            at_ms: 5,
        })
        .unwrap();
    let alt = s
        .put_alt_set(&AltSet {
            stable: None,
            prev: None,
            title: "Fee model".into(),
            question: None,
            members: vec![AltMember {
                label: "side".into(),
                view: "side".into(),
            }],
            status: AltStatus::Open,
            provenance: who("agent:a", Role::Writer),
        })
        .unwrap();
    let review = s
        .put_dismissal(&Review {
            subject: ReviewSubject::Tension {
                id: Hash([1; 32]),
                vkey: Hash([5; 32]),
                kind: "edit_collision".into(),
                severity: "blocking".into(),
                sides: vec!["canon".into()],
                blocks: vec![BlockId::parse("c-AAAAA").unwrap()],
            },
            case_hash: None,
            verdict: Verdict::Dismiss,
            reason: "fine".into(),
            citations: vec![],
            provenance: who("agent:r", Role::Reviewer),
        })
        .unwrap();
    // every object frame since creation: a versioned kind's is version 1, the others' are 0
    let mut written = std::collections::BTreeSet::new();
    let mut hashes = std::collections::BTreeSet::new();
    for b in &batches_of(&s)[created..] {
        for f in &b.frames {
            let Frame::Present {
                kind,
                hash,
                payload,
            } = f
            else {
                panic!("no frame is dropped here");
            };
            let o = codec::split(*kind, payload).unwrap();
            if o.kind == kinds::REF_UPDATE || o.kind == kinds::BATCH_COMMIT {
                assert_eq!(*kind, o.kind);
                continue;
            }
            let want = u16::from(versioned.contains(&o.kind));
            assert_eq!(o.version, want, "kind {}", o.kind);
            assert_eq!(*kind, codec::raw_kind(o.kind, want), "kind {}", o.kind);
            assert_eq!(*hash, Hash::of(payload));
            written.insert(o.kind);
            hashes.insert(*hash);
        }
    }
    for k in versioned {
        assert!(written.contains(&k), "a writer wrote kind {k}");
    }
    // each writer returns its frame's hash, and its refs name it
    for h in [
        side.head, proposal, landing, tag, answer, signature, blob, intent, alt, review,
    ] {
        assert!(hashes.contains(&h), "{h} is a written frame's hash");
    }
    assert_eq!(s.get_ref("tag/v1").unwrap(), Some(tag.0.to_vec()));
    let check = |s: &Store| {
        let t: Tag = s.get_typed(kinds::TAG, &tag).unwrap();
        assert_eq!(t.landing, landing);
        assert_eq!(s.landing(&landing).unwrap().proposal, proposal);
        assert_eq!(s.get_blob(&blob).unwrap(), b"a blob");
        assert_eq!(s.view_head("side").unwrap().unwrap().head, side.head);
        s.check_meta().unwrap();
        s.check_objects().unwrap();
    };
    check(&s);
    s.reindex().unwrap();
    check(&s);
    drop(s);
    let s = Store::open(&dir).unwrap();
    check(&s);
}
