//! Hub ports in the store. A port records its source in `links.ported_from` and takes change
//! encoding 3, or 4 when it embeds a block with a foreign part. A port's removal residue takes
//! residue encoding 2. The first committed batch that stores either turns on the incompatible
//! feature `ported_change`. The source is data: no edge, no dependency.
use crate::carried_fixture;

use rdy_model::encoding::{
    EncodedIdentity, LegacyChangeV0, LegacyRedactionResidueV0, PortChangeV3, PortForeignChangeV4,
    PortResidueV2, encoded_hash,
};
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::redaction::{
    CarriedSpan, ChangeResidue, FramePlace, ReasonClass, RedactTarget, RedactionCarried,
    RedactionResidue,
};
use rdy_model::*;
use rdy_store::bounded::RecordError;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::codec::{self, VERSIONED};
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, DumpWriter, RemovalTally};
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader, batch_end};
use rdy_store::format::{
    self, DumpStore, FEATURE_FOREIGN_COPY, FEATURE_PORTED_CHANGE, PORTED_CHANGE_REQUIRED_RDY,
    REF_STORE_FEATURES, StoreFeature, StoreFeatures, baseline_features, registered_features,
};
use rdy_store::ingest::Ingest;
use rdy_store::kinds;
use rdy_store::object_refs::{portable_refs_of, refs_of};
use rdy_store::sync_stream::StreamHeader;
use rdy_store::{Store, StoreError, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Provenance {
    Provenance {
        actor: "agent:w".into(),
        run: Some("w1".into()),
        role: Role::Writer,
        at_ms: 10,
    }
}

fn human() -> Provenance {
    Provenance {
        actor: "human:alice".into(),
        run: None,
        role: Role::Approver,
        at_ms: 50,
    }
}

fn block_of(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
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

/// A block with a foreign part: a link and a pin to a cited block of another repo.
fn foreign_block(id_: &str) -> Block {
    let target = ForeignId::new(RepoId([7; 16]), id("D-00001"));
    Block {
        foreign: ForeignParts {
            links: BTreeMap::from([(
                "depends_on".to_string(),
                vec![ForeignLink {
                    target: target.clone(),
                    part: None,
                }],
            )]),
            pins: BTreeMap::from([(
                target,
                ForeignPin {
                    content: h(44),
                    encoding: 0,
                },
            )]),
        },
        ..block_of(id_, "a citing claim")
    }
}

fn change_of(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: "a change".into(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// The source every port here names: no store here holds its change.
fn source() -> PortedFrom {
    PortedFrom {
        repo_id: RepoId([9; 16]),
        change: h(77),
    }
}

/// `c` as a port of [`source`].
fn port(mut c: Change) -> Change {
    c.links.ported_from = Some(source());
    c
}

fn plain_port(id_: &str) -> Change {
    port(change_of(vec![Op::Create {
        block: block_of(id_, "ported text"),
    }]))
}

/// A port that creates block `id_` and a block with a foreign part (`f-` for `c-`).
fn foreign_port(id_: &str) -> Change {
    port(change_of(vec![
        Op::Create {
            block: block_of(id_, "ported text"),
        },
        Op::Create {
            block: foreign_block(&id_.replace("c-", "f-")),
        },
    ]))
}

/// The registered `ported_change` feature.
fn ported_feature() -> StoreFeature {
    registered_features()
        .into_iter()
        .find(|f| f.name == FEATURE_PORTED_CHANGE)
        .expect("ported_change is registered")
}

fn version_of(item: &(u8, Vec<u8>)) -> u16 {
    codec::split(item.0, &item.1).unwrap().version
}

/// The features a frame uses, sorted.
fn features_of(item: &(u8, Vec<u8>)) -> Vec<String> {
    let mut out: Vec<String> = format::required_features(item.0, &item.1)
        .into_iter()
        .map(|f| f.name)
        .collect();
    out.sort();
    out
}

fn log_manifest(s: &Store) -> StoreFeatures {
    let v = s.reader().get_ref(REF_STORE_FEATURES).unwrap().unwrap();
    StoreFeatures::parse(&v).unwrap()
}

fn names(f: &StoreFeatures) -> Vec<&str> {
    f.features.iter().map(|f| f.name.as_str()).collect()
}

/// Whether the last sealed batch of `s` sets the features ref.
fn last_batch_sets_features(s: &Store) -> bool {
    let mut last = None;
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            last = Some(b);
            Ok(())
        })
        .unwrap();
    last.unwrap().frames.iter().any(|f| {
        f.kind() == kinds::REF_UPDATE
            && f.present_payload()
                .and_then(|p| postcard::from_bytes::<kinds::RefUpdate>(p).ok())
                .is_some_and(|u| u.name == REF_STORE_FEATURES)
    })
}

/// A residue's frame as the store writes a new one: in the encoding its value chooses.
fn residue_frame(r: &RedactionResidue) -> (u8, Vec<u8>) {
    codec::item_new(kinds::REDACTION_RESIDUE, r)
}

fn residue_hash(r: &RedactionResidue) -> Hash {
    Hash::of(&residue_frame(r).1)
}

fn payload(version: u16, body: &[u8]) -> Vec<u8> {
    let mut p = version.to_le_bytes().to_vec();
    p.extend_from_slice(body);
    p
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

/// The sealed batches of the store at `dir`.
fn batches_of(dir: &Path) -> Vec<DumpBatch> {
    let s = Store::open(dir).unwrap();
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

/// A store at `dir` that commits `changes` to view `work`, lands them and tags `v1`: the tag.
fn landed(dir: &Path, changes: Vec<Change>) -> (Vec<Hash>, Hash) {
    let mut s = Store::create(dir).unwrap();
    let ids: Vec<Hash> = changes
        .into_iter()
        .map(|c| s.commit_to_view("work", c).unwrap())
        .collect();
    let p = s.propose("work", "carry", writer()).unwrap();
    let l = s.land(&p, human()).unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    (ids, tag)
}

#[test]
fn hpt1_ported_from_is_not_an_object_edge() {
    let all = StoreFeatures::of(registered_features());
    // a port's edges are its unported twin's: the source adds none
    for (ported, twin) in [
        (plain_port("c-00001"), {
            let mut c = plain_port("c-00001");
            c.links.ported_from = None;
            c
        }),
        (foreign_port("c-00002"), {
            let mut c = foreign_port("c-00002");
            c.links.ported_from = None;
            c
        }),
    ] {
        let p = codec::item(kinds::CHANGE, &ported).unwrap();
        let t = codec::item(kinds::CHANGE, &twin).unwrap();
        let edges = refs_of(&all, p.0, &p.1).unwrap();
        assert_eq!(edges, refs_of(&all, t.0, &t.1).unwrap());
        assert!(edges.iter().all(|r| r.hash != source().change));
        assert_eq!(
            portable_refs_of(&all, p.0, &p.1).unwrap(),
            portable_refs_of(&all, t.0, &t.1).unwrap()
        );
    }
    // a replayed port has its replay edge, and still no source edge
    let mut replayed = plain_port("c-00003");
    replayed.links.replayed_from = Some(h(5));
    let r = codec::item(kinds::CHANGE, &replayed).unwrap();
    let edges = refs_of(&all, r.0, &r.1).unwrap();
    assert!(edges.iter().any(|e| e.hash == h(5)));
    assert!(edges.iter().all(|e| e.hash != source().change));

    // the store holds no source change: the checks and the export need none
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("s");
    let (ids, tag) = landed(&dir, vec![plain_port("c-00001")]);
    let s = Store::open(&dir).unwrap();
    assert!(!s.reader().has(&source().change).unwrap());
    s.verify().unwrap();
    s.check_objects().unwrap();
    let r = s.reader();
    let cl = closure(&r, tag, &BTreeSet::new()).unwrap();
    assert!(cl.records.iter().any(|k| k.hash == ids[0]));
    assert!(cl.records.iter().all(|k| k.hash != source().change));
    assert!(cl.prerequisites.is_empty(), "{:?}", cl.prerequisites);
}

#[test]
fn hpt1_feature_appears_in_the_first_committed_port_batch() {
    let ported_change = ported_feature();
    assert_eq!(ported_change.class, format::FeatureClass::Incompatible);
    assert_eq!(ported_change.required_rdy, PORTED_CHANGE_REQUIRED_RDY);
    assert_eq!(PORTED_CHANGE_REQUIRED_RDY, "0.58.0");
    assert_eq!(
        format::required_rdy(FEATURE_PORTED_CHANGE),
        PORTED_CHANGE_REQUIRED_RDY
    );
    let d = tempfile::tempdir().unwrap();
    // an untouched fresh store does not use it
    let dir = d.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    assert!(!s.has_feature_now(FEATURE_PORTED_CHANGE));
    // unported changes do not turn it on
    s.commit_to_view(
        "work",
        change_of(vec![Op::Create {
            block: block_of("c-00009", "local"),
        }]),
    )
    .unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    // a failed port batch writes nothing and turns nothing on: it updates a block the view
    // does not hold
    let broken = port(change_of(vec![Op::Update {
        base: h(200),
        block: block_of("c-00007", "no base"),
    }]));
    let end = s.log_end();
    s.commit_to_view("work", broken).unwrap_err();
    assert_eq!(s.log_end(), end);
    assert!(!log_manifest(&s).has(FEATURE_PORTED_CHANGE));
    assert!(!s.has_feature_now(FEATURE_PORTED_CHANGE));
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(baseline_features())
    );
    // a valid port whose append fails also writes nothing and turns nothing on
    s.fail_next_append_after(40);
    s.commit_to_view("work", plain_port("c-00001")).unwrap_err();
    assert_eq!(s.log_end(), end);
    assert!(!log_manifest(&s).has(FEATURE_PORTED_CHANGE));
    assert!(!s.has_feature_now(FEATURE_PORTED_CHANGE));
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(baseline_features())
    );
    // the first committed port turns it on in its own sealed batch, and FORMAT mirrors it
    let p = plain_port("c-00001");
    let item = codec::item(kinds::CHANGE, &p).unwrap();
    assert_eq!(version_of(&item), 3);
    let got = s.commit_to_view("work", p.clone()).unwrap();
    assert_eq!(got, p.id());
    assert_eq!(got, Hash::of(&item.1));
    let manifest = log_manifest(&s);
    assert_eq!(
        names(&manifest),
        [
            "content_hash_v2",
            "dedup_spans",
            "object_encoding",
            "ported_change",
            "snapshot_delta"
        ]
    );
    assert!(last_batch_sets_features(&s), "in the port's batch");
    assert!(s.has_feature_now(FEATURE_PORTED_CHANGE));
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(manifest.clone())
    );
    // the read keeps the source and the stored identity
    let read = s.change(&got).unwrap();
    assert_eq!(read.links.ported_from, Some(source()));
    assert_eq!(
        read.read_identity,
        Some(ChangeReadIdentity::Stored(EncodedIdentity {
            hash: got,
            version: 3
        }))
    );
    // a second port moves nothing; a reopen keeps the feature
    s.commit_to_view("work", plain_port("c-00002")).unwrap();
    assert!(!last_batch_sets_features(&s));
    drop(s);
    let s = Store::open(&dir).unwrap();
    assert_eq!(log_manifest(&s), manifest);
    drop(s);
    // a first use whose batch never sealed: the open puts FORMAT back to the log's manifest
    let dir = d.path().join("torn");
    drop(Store::create(&dir).unwrap());
    let ahead = baseline_features().merged(std::slice::from_ref(&ported_change));
    format::write_format_file(&dir, &ahead).unwrap();
    carried_fixture::append_raw(&dir, &[item], false);
    let s = Store::open(&dir).unwrap();
    assert_eq!(log_manifest(&s), baseline_features());
    assert!(!s.has_feature_now(FEATURE_PORTED_CHANGE));
    assert_eq!(
        format::read_format_file(&dir).unwrap(),
        Some(baseline_features())
    );
}

#[test]
fn hpt1_foreign_port_needs_both_features() {
    // a port with a foreign part: encoding 4, both features, in one batch
    let fp = foreign_port("c-00001");
    let item = codec::item(kinds::CHANGE, &fp).unwrap();
    assert_eq!(version_of(&item), 4);
    assert_eq!(
        features_of(&item),
        ["foreign_copy", "object_encoding", "ported_change"]
    );
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("foreign");
    let mut s = Store::create(&dir).unwrap();
    s.commit_to_view("work", fp.clone()).unwrap();
    let m = log_manifest(&s);
    assert!(m.has(FEATURE_PORTED_CHANGE) && m.has(FEATURE_FOREIGN_COPY));
    assert!(last_batch_sets_features(&s));
    assert_eq!(
        s.change(&fp.id()).unwrap().links.ported_from,
        Some(source())
    );
    drop(s);

    // the inverse: a plain port is encoding 3 and needs ported_change alone
    let pp = plain_port("c-00002");
    let item = codec::item(kinds::CHANGE, &pp).unwrap();
    assert_eq!(version_of(&item), 3);
    assert_eq!(features_of(&item), ["object_encoding", "ported_change"]);
    let dir = d.path().join("plain");
    let mut s = Store::create(&dir).unwrap();
    s.commit_to_view("work", pp).unwrap();
    let m = log_manifest(&s);
    assert!(m.has(FEATURE_PORTED_CHANGE));
    assert!(
        !m.has(FEATURE_FOREIGN_COPY),
        "a plain port is no foreign copy"
    );
    drop(s);

    // an unported change with a foreign part keeps encoding 2 and foreign_copy alone
    let mut unported = foreign_port("c-00003");
    unported.links.ported_from = None;
    let item = codec::item(kinds::CHANGE, &unported).unwrap();
    assert_eq!(version_of(&item), 2);
    assert_eq!(features_of(&item), ["foreign_copy", "object_encoding"]);

    // an rdy that does not know the feature refuses to open the store
    let _older = format::older_reader_for_tests(&[FEATURE_PORTED_CHANGE]);
    match Store::open(&dir) {
        Err(StoreError::Format { message, fix }) => {
            assert_eq!(message, "store feature ported_change needs rdy 0.58.0");
            assert_eq!(fix, "open the project with rdy 0.58.0 or later");
        }
        other => panic!("not the format refusal: {:?}", other.map(|_| ())),
    }
}

#[test]
fn hpt1_port_frames_refuse_wrong_shapes() {
    let pp = plain_port("c-00001");
    let fp = foreign_port("c-00002");
    let decodes = |version: u16, body: &[u8]| {
        let raw = codec::raw_kind(kinds::CHANGE, version);
        let p = payload(version, body);
        (
            codec::decode_record::<Change>(raw, &p).is_ok(),
            codec::decode_bounded::<Change>(raw, &p).is_ok(),
        )
    };
    // each form reads in its own encoding
    let body3 = enc(&PortChangeV3::of(&pp));
    let body4 = enc(&PortForeignChangeV4::of(&fp));
    assert_eq!(decodes(3, &body3), (true, true));
    assert_eq!(decodes(4, &body4), (true, true));
    // encoding 3 or 4 with no port source
    let mut unported = PortChangeV3::of(&pp);
    unported.links.ported_from = None;
    assert_eq!(decodes(3, &enc(&unported)), (false, false));
    let mut unported = PortForeignChangeV4::of(&fp);
    unported.links.ported_from = None;
    assert_eq!(decodes(4, &enc(&unported)), (false, false));
    // the wrong shape for the encoding: a plain port's body in 4, a foreign port's in 3, a port
    // body in 1 or 2
    assert_eq!(
        decodes(4, &enc(&PortForeignChangeV4::of(&pp))),
        (false, false)
    );
    assert!(!decodes(3, &body4).1);
    assert!(!decodes(1, &body3).1);
    assert!(!decodes(2, &body4).1);
    // a malformed length: the body cut short, or no version bytes at all
    assert_eq!(decodes(3, &body3[..body3.len() - 1]), (false, false));
    assert_eq!(
        codec::split(codec::raw_kind(kinds::CHANGE, 3), &[3]),
        Err(RecordError::Decode)
    );
    // trailing bytes: the bounded read takes exactly one value, and no canonical form has them
    let mut trailing = body3.clone();
    trailing.push(0);
    assert!(!decodes(3, &trailing).1);
    assert!(
        codec::decode_canonical::<Change>(
            codec::raw_kind(kinds::CHANGE, 3),
            &payload(3, &trailing)
        )
        .is_err()
    );
    // the canonical check holds each frame to its value's own encoding
    let p3 = payload(3, &body3);
    let o = codec::split(codec::raw_kind(kinds::CHANGE, 3), &p3).unwrap();
    let read: Change = codec::decode_split(&o).unwrap();
    assert!(codec::is_canonical(&o, &read));
    let mut unported_read = read.clone();
    unported_read.links.ported_from = None;
    unported_read.read_identity = None;
    assert!(!codec::is_canonical(&o, &unported_read));
    // an altered stored read keeps the identity of its frame: refused before any write
    let mut altered = read.clone();
    altered.message = "edited".into();
    match codec::encode(kinds::CHANGE, &altered) {
        Err(StoreError::Refused(m)) => assert_eq!(m, codec::IDENTITY_MISMATCH),
        other => panic!("not the identity refusal: {other:?}"),
    }
    let mut repointed = read.clone();
    repointed.links.ported_from = Some(PortedFrom {
        change: h(78),
        ..source()
    });
    assert!(codec::encode(kinds::CHANGE, &repointed).is_err());
    // an unchanged stored read writes its own bytes
    let e = codec::encode(kinds::CHANGE, &read).unwrap();
    assert_eq!(e.payload, payload(3, &body3));

    // residues: encoding 2 only for a port's residue, encoding 1 never for one
    let port_residue = RedactionResidue::Change(ChangeResidue::of(&pp));
    let body2 = enc(&PortResidueV2::of(&port_residue).unwrap());
    let residue_reads = |version: u16, body: &[u8]| {
        let raw = codec::raw_kind(kinds::REDACTION_RESIDUE, version);
        let p = payload(version, body);
        (
            codec::decode_record::<RedactionResidue>(raw, &p).is_ok(),
            codec::decode_bounded::<RedactionResidue>(raw, &p).is_ok(),
        )
    };
    assert_eq!(residue_reads(2, &body2), (true, true));
    let empty = PortResidueV2 {
        residue: RedactionResidue::Change(ChangeResidue::of(&change_of(vec![]))),
        ported_from: None,
    };
    assert_eq!(residue_reads(2, &enc(&empty)), (false, false));
    let block_in_two = PortResidueV2 {
        residue: RedactionResidue::Block(rdy_model::redaction::BlockResidue::of(&block_of(
            "c-00001", "x",
        ))),
        ported_from: Some(source()),
    };
    assert_eq!(residue_reads(2, &enc(&block_in_two)), (false, false));
    // a port's source after an encoding-1 body: a trailing run the bounded read refuses
    let mut v1_with_source = enc(&RedactionResidue::Change(ChangeResidue::of(&change_of(
        vec![],
    ))));
    v1_with_source.push(1);
    v1_with_source.extend([9u8; 16]);
    v1_with_source.extend(h(77).0);
    assert!(!residue_reads(1, &v1_with_source).1);
    assert_eq!(codec::residue_body(&port_residue, 1), None);
    assert_eq!(codec::residue_body(&port_residue, 0), None);
    assert_eq!(codec::residue_body(&port_residue, 2), Some(body2.clone()));
    assert!(!residue_reads(2, &body2[..body2.len() - 1]).1);

    // unknown future encodings: CHANGE 5 and REDACTION_RESIDUE 3
    for (kind, name, version) in [
        (kinds::CHANGE, "CHANGE", 5u16),
        (kinds::REDACTION_RESIDUE, "REDACTION_RESIDUE", 3),
    ] {
        let raw = kind | VERSIONED;
        let p = payload(version, b"any body");
        assert_eq!(
            codec::split(raw, &p).unwrap_err(),
            RecordError::UnknownVersion
        );
        match codec::decode_record::<Change>(raw, &p) {
            Err(StoreError::Format { message, .. }) => assert_eq!(
                message,
                format!(
                    "the store holds an object this rdy cannot read: kind {name}, encoding {version}"
                )
            ),
            other => panic!("{name}: not the unknown-encoding refusal: {other:?}"),
        }
    }
    assert_eq!(codec::newest(kinds::CHANGE), 4);
    assert_eq!(codec::newest(kinds::REDACTION_RESIDUE), 2);
    assert_eq!(
        codec::latest(kinds::CHANGE),
        1,
        "a new unported change keeps 1"
    );
    assert_eq!(codec::latest(kinds::REDACTION_RESIDUE), 1);
    assert_eq!(
        codec::PORT_ENCODINGS,
        &[
            (kinds::CHANGE, 3),
            (kinds::CHANGE, 4),
            (kinds::REDACTION_RESIDUE, 2)
        ]
    );
}

/// The sync file of the closure of `tag` in `reader`.
fn stream_of(reader: &StoreReader, tag: Hash) -> Vec<u8> {
    let c = closure(reader, tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        reader.store_features().unwrap(),
        reader.repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites,
    );
    let mut source = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &c.records, Vec::new())
        .unwrap()
        .0
}

fn incoming(
    _: &mut dyn rdy_store::sync_proof::ObjectSource,
    _: &rdy_store::take_batch::UploadFacts,
) -> Result<rdy_store::take_batch::TagChoice, rdy_store::stream_error::StreamError> {
    Ok(rdy_store::take_batch::TagChoice::Incoming)
}

/// A new holder at `holder` that took the history of `tag` of the store at `src`.
pub(crate) fn take_into(src: &Path, holder: &Path, tag: Hash) -> Store {
    let s = Store::open(src).unwrap();
    let r = s.reader();
    let file = holder.with_extension("rdysync");
    std::fs::write(&file, stream_of(&r, tag)).unwrap();
    let t: Tag = r.get_typed(kinds::TAG, &tag).unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    drop(r);
    drop(s);
    let mut h = Store::create_holder(holder, repo).unwrap();
    h.take_batches(
        &[std::fs::File::open(&file).unwrap()],
        &rdy_store::take_batch::UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
        incoming,
    )
    .unwrap();
    h
}

/// The full dump of the store at `src`, loaded into `to`.
fn dump_and_load(src: &Path, to: &Path) {
    let (repo, manifest) = {
        let s = Store::open(src).unwrap();
        (s.repo_id().unwrap().unwrap(), log_manifest(&s))
    };
    let batches = batches_of(src);
    let last = batches.last().unwrap();
    let header = rdy_store::dump::DumpHeader {
        store: DumpStore::Features(manifest.clone()),
        log_end: batch_end(last).unwrap(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, BatchMarker::of(last))),
    };
    let mut tally = RemovalTally::default();
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in &batches {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    let bytes = w.finish(tally.facts().unwrap()).unwrap().0;
    let (mut reader, _, _) = DumpReader::open(&bytes[..]).unwrap();
    let mut read = vec![];
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            read.push(b);
        }
    }
    assert_eq!(read, batches);
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in read.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(&DumpStore::Features(manifest)).unwrap();
    i.finish().unwrap();
}

#[test]
fn hpt1_port_forms_survive_reopen_reindex_dump_and_stream() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    let (ids, tag) = landed(&src, vec![plain_port("c-00001"), foreign_port("c-00002")]);
    let frames_of = |s: &Store| -> Vec<(u8, Vec<u8>)> {
        ids.iter()
            .map(|h| s.reader().get_kind_raw(kinds::CHANGE, h).unwrap())
            .collect()
    };
    let (want, manifest) = {
        let s = Store::open(&src).unwrap();
        (frames_of(&s), log_manifest(&s))
    };
    assert_eq!(
        want.iter().map(version_of).collect::<Vec<_>>(),
        [3, 4],
        "each form in its own encoding"
    );
    for (f, h) in want.iter().zip(&ids) {
        assert_eq!(Hash::of(&f.1), *h);
    }
    assert!(manifest.has(FEATURE_PORTED_CHANGE) && manifest.has(FEATURE_FOREIGN_COPY));
    let check = |s: &Store, what: &str| {
        assert_eq!(frames_of(s), want, "{what}: the same frames");
        assert_eq!(log_manifest(s), manifest, "{what}: the same features");
        for h in &ids {
            let c = s.change(h).unwrap();
            assert_eq!(c.links.ported_from, Some(source()), "{what}");
            assert_eq!(c.id(), *h, "{what}");
        }
    };
    // reopen and reindex
    let mut s = Store::open(&src).unwrap();
    check(&s, "reopen");
    s.reindex().unwrap();
    check(&s, "reindex");
    drop(s);
    // dump and load
    let loaded = d.path().join("loaded");
    dump_and_load(&src, &loaded);
    let s = Store::open(&loaded).unwrap();
    check(&s, "dump and load");
    drop(s);
    // the sync stream into a holder: the same frames, and the holder needs both features
    let holder = take_into(&src, &d.path().join("holder"), tag);
    assert_eq!(frames_of(&holder), want, "stream: the same frames");
    let hm = log_manifest(&holder);
    assert!(hm.has(FEATURE_PORTED_CHANGE) && hm.has(FEATURE_FOREIGN_COPY));
}

#[test]
fn hpr1_residue_only_batch_activates_ported_change() {
    // the residue of a port uses ported_change alone, whatever its source encoding: the residue
    // of an encoding-4 source does not use foreign_copy
    for (c, source_encoding) in [(plain_port("c-00001"), 3), (foreign_port("c-00002"), 4)] {
        let r = RedactionResidue::Change(ChangeResidue::of(&c));
        let item = residue_frame(&r);
        assert_eq!(version_of(&item), 2);
        let RedactionResidue::Change(cr) = &r else {
            unreachable!()
        };
        assert_eq!(cr.source_encoding, source_encoding);
        assert_eq!(features_of(&item), ["object_encoding", "ported_change"]);
    }
    // an unported change's residue uses no port feature
    let r = RedactionResidue::Change(ChangeResidue::of(&change_of(vec![])));
    assert_eq!(version_of(&residue_frame(&r)), 1);
    assert_eq!(features_of(&residue_frame(&r)), ["object_encoding"]);

    // a holder that takes a removed port holds no port frame, only its residue: the take's
    // batch turns the feature on, for a plain and an encoding-4 source alike
    let d = tempfile::tempdir().unwrap();
    let mut tags = vec![];
    for (n, c) in [plain_port("c-00001"), foreign_port("c-00002")]
        .into_iter()
        .enumerate()
    {
        let src = d.path().join(format!("src-{n}"));
        let (ch, tag) = removed_port(&src, c);
        tags.push(tag);
        let holder = d.path().join(format!("holder-{n}"));
        let taken = take_into(&src, &holder, tag);
        assert!(
            taken.reader().get_kind_raw(kinds::CHANGE, &ch).is_err(),
            "case {n}: the port's frame is dropped"
        );
        let projected = taken.change(&ch).unwrap();
        assert!(projected.is_projection(), "case {n}");
        assert_eq!(projected.links.ported_from, Some(source()), "case {n}");
        assert!(log_manifest(&taken).has(FEATURE_PORTED_CHANGE), "case {n}");
        assert!(taken.has_feature_now(FEATURE_PORTED_CHANGE), "case {n}");
        assert!(
            last_batch_sets_features(&taken),
            "case {n}: in the take's batch"
        );
    }

    // a stopped load counts the residue it appends: a source log whose manifest does not name
    // the feature, and whose port frame the load drops, is refused before it appends anything
    let src = d.path().join("src-hidden");
    let ch = raw_source(&src, &plain_port("c-00003"));
    let (r, rec) = dropped_change(&src, ch);
    match carried_load(
        &src,
        &d.path().join("to-hidden"),
        std::slice::from_ref(&r),
        &rec,
        &baseline_features(),
    ) {
        Err(StoreError::Dump { what, .. }) => assert_eq!(
            what,
            "the dump's records use feature ported_change, which its manifest does not name"
        ),
        other => panic!("a hidden ported_change was kept: {other:?}"),
    }

    // a failed carried batch stores no residue and uses nothing: its span names another residue
    let src = d.path().join("src-failed");
    let ch = committed_source(&src, plain_port("c-00004"));
    let (r, mut rec) = dropped_change(&src, ch);
    rec.spans[0].residue = Some(h(201));
    let batches = carried_fixture::batches_with_drops(&src, std::slice::from_ref(&rec));
    let mut i = Ingest::create(&d.path().join("to-failed")).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    let end = i.end();
    assert!(
        i.append_carried(std::slice::from_ref(&r), std::slice::from_ref(&rec))
            .is_err()
    );
    assert_eq!(i.end(), end, "nothing appended");
    // a failed take stores nothing and turns nothing on: its target names another landing
    let src = d.path().join("src-0");
    let file = d.path().join("failed.rdysync");
    let repo = {
        let s = Store::open(&src).unwrap();
        std::fs::write(&file, stream_of(&s.reader(), tags[0])).unwrap();
        s.repo_id().unwrap().unwrap()
    };
    let mut held = Store::create_holder(&d.path().join("holder-failed"), repo).unwrap();
    let end = held.log_end();
    assert!(
        held.take_batches(
            &[std::fs::File::open(&file).unwrap()],
            &rdy_store::take_batch::UploadTarget {
                tag: "v1".into(),
                landing: h(202),
            },
            incoming,
        )
        .is_err()
    );
    assert_eq!(held.log_end(), end, "nothing appended");
    assert!(!log_manifest(&held).has(FEATURE_PORTED_CHANGE));
    assert!(!held.has_feature_now(FEATURE_PORTED_CHANGE));
}

#[test]
fn hpr1_carried_residue_preserves_named_encoding() {
    let d = tempfile::tempdir().unwrap();
    // a new carried proof of a port names its encoding-2 residue: the load stores it as named
    let src = d.path().join("src-port");
    let ch = committed_source(&src, plain_port("c-00001"));
    let (r, rec) = dropped_change(&src, ch);
    let named = rec.spans[0].residue.unwrap();
    assert_eq!(
        named,
        Hash::of(
            &codec::frame(
                kinds::REDACTION_RESIDUE,
                2,
                codec::residue_body(&r, 2).unwrap()
            )
            .unwrap()
            .1
        )
    );
    let manifest = log_manifest(&Store::open(&src).unwrap());
    assert!(manifest.has(FEATURE_PORTED_CHANGE));
    let to = d.path().join("to-port");
    carried_load(&src, &to, std::slice::from_ref(&r), &rec, &manifest).unwrap();
    assert_eq!(stored_residue(&to, named), (2, r.clone()));

    // an unported change, named by a new proof in encoding 1: stored in encoding 1
    let src = d.path().join("src-new");
    let ch = raw_source(
        &src,
        &change_of(vec![Op::Create {
            block: block_of("c-00002", "new text"),
        }]),
    );
    let (r1, rec) = dropped_change(&src, ch);
    assert_eq!(version_of(&residue_frame(&r1)), 1);
    let to = d.path().join("to-new");
    carried_load(
        &src,
        &to,
        std::slice::from_ref(&r1),
        &rec,
        &baseline_features(),
    )
    .unwrap();
    assert_eq!(stored_residue(&to, residue_hash(&r1)), (1, r1.clone()));
    assert!(!log_manifest(&Store::open(&to).unwrap()).has(FEATURE_PORTED_CHANGE));

    // an encoding-0 change, named by an old proof in encoding 0: stored in encoding 0, with its
    // old bytes; named in encoding 1 by a new proof: stored in encoding 1
    let src = d.path().join("src-zero");
    drop(Store::create(&src).unwrap());
    let old_change = change_of(vec![Op::Create {
        block: block_of("c-00003", "older text"),
    }]);
    let old_body = enc(&LegacyChangeV0::from_current(&old_change).unwrap());
    carried_fixture::append_raw(&src, &[(kinds::CHANGE, old_body.clone())], true);
    let old_id = Hash::of(&old_body);
    let stored = Store::open(&src).unwrap().change(&old_id).unwrap();
    let r0 = RedactionResidue::Change(ChangeResidue::of(&stored));
    let RedactionResidue::Change(c0) = &r0 else {
        unreachable!()
    };
    assert_eq!(c0.source_encoding, 0);
    let body0 = codec::residue_body(&r0, 0).unwrap();
    assert_eq!(
        body0,
        enc(&LegacyRedactionResidueV0::from_current(&r0).unwrap())
    );
    let named0 = encoded_hash(0, &body0);
    assert_ne!(named0, residue_hash(&r0), "encoding 1 is another frame");
    let f = carried_fixture::find(&carried_fixture::framed_at(&src), kinds::CHANGE, old_id);
    for (name, hash, version) in [("zero", named0, 0u16), ("one", residue_hash(&r0), 1)] {
        let rec = carried(vec![], vec![span_of(&f, Some(hash), false)]);
        let to = d.path().join(format!("to-{name}"));
        carried_load(
            &src,
            &to,
            std::slice::from_ref(&r0),
            &rec,
            &baseline_features(),
        )
        .unwrap();
        assert_eq!(stored_residue(&to, hash), (version, r0.clone()), "{name}");
    }
}

/// A carried record of the source's removal, over `spans`.
fn carried(targets: Vec<RedactTarget>, spans: Vec<CarriedSpan>) -> RedactionCarried {
    RedactionCarried {
        request: carried_fixture::source_request(),
        apply: carried_fixture::source_apply(),
        landing: carried_fixture::source_landing(),
        approved_by: carried_fixture::approver(),
        reason_class: ReasonClass::Secret,
        targets,
        spans,
    }
}

/// The span of frame `f`, naming residue `residue`.
fn span_of(f: &carried_fixture::Framed, residue: Option<Hash>, held: bool) -> CarriedSpan {
    CarriedSpan {
        place: FramePlace {
            segment: f.loc.segment,
            offset: f.loc.offset,
        },
        kind: f.kind,
        hash: f.hash,
        len: f.len,
        residue,
        held,
    }
}

/// A source store at `dir` that holds change `c` as a raw frame of its own batch, so no batch
/// of the source turns a feature on for it: the change's id.
fn raw_source(dir: &Path, c: &Change) -> Hash {
    drop(Store::create(dir).unwrap());
    let item = codec::item(kinds::CHANGE, c).unwrap();
    carried_fixture::append_raw(dir, std::slice::from_ref(&item), true);
    Hash::of(&item.1)
}

/// A source store at `dir` whose view `work` commits `c`: the stored change's id.
fn committed_source(dir: &Path, c: Change) -> Hash {
    let mut s = Store::create(dir).unwrap();
    s.commit_to_view("work", c).unwrap()
}

/// A source store at `dir` whose view `work` commits port `c`, lands it, removes the version of
/// its first block through an approved request and tags the removal `v1`: the port's id and the
/// tag.
fn removed_port(dir: &Path, c: Change) -> (Hash, Hash) {
    use rdy_model::redaction::{
        AssessmentSupport, BlockResidue, DropSpan, Redaction, RedactionManifest,
    };
    let Op::Create { block: b } = &c.ops[0] else {
        unreachable!()
    };
    let b = b.clone();
    let mut s = Store::create(dir).unwrap();
    let ch = s.commit_to_view("work", c).unwrap();
    // the request: the block frame and its producers, each named by the residue frame the
    // residue's value chooses
    let reader = s.reader();
    let v = b.version_hash();
    let producers: BTreeSet<Hash> = reader.producers(&v).unwrap().into_iter().collect();
    let mut spans = vec![];
    let mut residues = vec![];
    for f in crate::redaction_fixture::frames(&reader, s.log_end()) {
        let r = match f.kind {
            kinds::BLOCK if f.hash == v => RedactionResidue::Block(BlockResidue::of(&b)),
            kinds::CHANGE if producers.contains(&f.hash) => {
                RedactionResidue::Change(ChangeResidue::of(&reader.change(&f.hash).unwrap()))
            }
            _ => continue,
        };
        spans.push(DropSpan {
            place: FramePlace {
                segment: f.loc.segment,
                offset: f.loc.offset,
            },
            kind: f.kind,
            hash: f.hash,
            len: f.len,
            residue: Some(residue_hash(&r)),
        });
        residues.push(r);
    }
    drop(reader);
    let requester = Provenance {
        actor: "agent:requester".into(),
        run: Some("q1".into()),
        role: Role::Writer,
        at_ms: 40,
    };
    let carrier = rdy_model::redaction::carrier("work".into(), vec![], requester.clone());
    let request = Redaction {
        carrier: carrier.id(),
        actor: requester,
        targets: vec![RedactTarget::Version {
            id: b.id.clone(),
            version: v,
        }],
        reason_class: ReasonClass::Secret,
        reason: "a leaked key".into(),
        manifest: RedactionManifest {
            versions: vec![(b.id.clone(), v)],
            blobs: vec![],
            shared_blobs: vec![],
            spans,
            inside: vec![],
            outside: crate::redaction_fixture::outside_rows(),
            quarantine: vec![],
            assessment: AssessmentSupport::NotImplemented,
        },
    };
    let p = s.prepare_redaction(&carrier, &request, &residues).unwrap();
    s.commit_redaction(p).unwrap();
    let prop = s.propose("work", "carry", writer()).unwrap();
    let landing = s
        .land_atomic(rdy_store::LandRequest {
            proposal: prop,
            accept: None,
            approver: human(),
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap()
        .landing;
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing,
            message: None,
            tagged_by: human(),
        })
        .unwrap();
    (ch, tag)
}

/// The residue of change `ch` of the source at `src`, and a carried record that drops the
/// change's frame and names that residue in the encoding its value chooses.
fn dropped_change(src: &Path, ch: Hash) -> (RedactionResidue, RedactionCarried) {
    let stored = Store::open(src).unwrap().change(&ch).unwrap();
    let r = RedactionResidue::Change(ChangeResidue::of(&stored));
    let f = carried_fixture::find(&carried_fixture::framed_at(src), kinds::CHANGE, ch);
    let rec = carried(vec![], vec![span_of(&f, Some(residue_hash(&r)), false)]);
    (r, rec)
}

/// The stopped load of the source at `src` into `to`: its batches with `record`'s `held: false`
/// spans dropped, then the carried batch of `residues` and `record`, then the upgrade under
/// `manifest`.
fn carried_load(
    src: &Path,
    to: &Path,
    residues: &[RedactionResidue],
    record: &RedactionCarried,
    manifest: &StoreFeatures,
) -> Result<(), StoreError> {
    let batches = carried_fixture::batches_with_drops(src, std::slice::from_ref(record));
    let mut i = Ingest::create(to)?;
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b)?;
    }
    i.append_carried(residues, std::slice::from_ref(record))?;
    i.upgrade(&DumpStore::Features(manifest.clone()))?;
    i.finish()?;
    Ok(())
}

/// The residue frame of hash `h` in the store at `dir`: its encoding and the residue.
fn stored_residue(dir: &Path, h: Hash) -> (u16, RedactionResidue) {
    let s = Store::open(dir).unwrap();
    let (raw, p) = s
        .reader()
        .get_kind_raw(kinds::REDACTION_RESIDUE, &h)
        .unwrap();
    let o = codec::split(raw, &p).unwrap();
    (o.version, codec::decode_split(&o).unwrap())
}
