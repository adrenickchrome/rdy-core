//! r59017 (D-GGTYA, D-4PV25): sync stream format 5, the canon state. Byte 4 is `canon_state`
//! in format 5 alone; formats 2 to 4 and their fixtures are unchanged; a canon state is a
//! download, so no history take of a holder or a project admits it.
use rdy_model::*;
use rdy_store::canon_state::{StateSelection, select_state};
use rdy_store::closure::{END_OF_LOG, history_closure_before, write_batch};
use rdy_store::format::FEATURE_PARTIAL_HISTORY;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{
    CANON_STATE_FORMAT, HISTORY_FORMAT, READ_FORMATS, StreamHeader, StreamPurpose, StreamReader,
    StreamWriter, check_batch, check_canon_state_batch,
};
use rdy_store::take_batch::{FetchTarget, HistoryTarget, TakeError, UploadTarget};
use rdy_store::{LandRequest, Store, StoreReader};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::{Path, PathBuf};

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: if role == Role::Approver {
            "human:a".into()
        } else {
            "agent:w".into()
        },
        run: None,
        role,
        at_ms: 1,
    }
}

fn block(n: u32) -> Block {
    Block {
        id: BlockId::parse(&format!("b-{n:05}")).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: format!("claim {n}"),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// Lands one change that creates block `n` through the store's own landing path.
fn land(s: &mut Store, n: u32) -> Hash {
    s.commit_to_view(
        "work",
        Change {
            view: "work".into(),
            ops: vec![Op::Create { block: block(n) }],
            deps: vec![],
            provenance: prov(Role::Writer),
            justification: None,
            message: format!("change {n}"),
            links: ChangeLinks::default(),
            read_identity: None,
        },
    )
    .unwrap();
    let p = s
        .propose("work", &format!("p{n}"), prov(Role::Writer))
        .unwrap();
    s.land_atomic(LandRequest {
        proposal: p,
        accept: None,
        approver: prov(Role::Approver),
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    })
    .unwrap()
    .landing
}

/// The canon state of `root` as one batch: its bytes, header and selection.
fn state(reader: &StoreReader, root: Hash) -> (Vec<u8>, StreamHeader, StateSelection) {
    let sel = select_state(reader, root, END_OF_LOG).unwrap();
    let header = StreamHeader::canon_state(
        sel.features.clone(),
        reader.repo_id().unwrap().unwrap(),
        root,
        sel.prerequisites.clone(),
    );
    let mut source = sel.source(reader, END_OF_LOG);
    let (bytes, _) = write_batch(&mut source, &header, &sel.records, Vec::new()).unwrap();
    (bytes, header, sel)
}

/// The canon history of `root` as one format-4 batch.
fn canon_history(reader: &StoreReader, root: Hash) -> Vec<u8> {
    let found = history_closure_before(
        reader,
        StreamPurpose::CanonHistory,
        root,
        &BTreeSet::new(),
        None,
        END_OF_LOG,
    )
    .unwrap();
    let header = StreamHeader::canon_history(
        found.features,
        reader.repo_id().unwrap().unwrap(),
        root,
        found.prerequisites,
    );
    let mut source = rdy_store::closure::StoreSource {
        reader,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &found.records, Vec::new())
        .unwrap()
        .0
}

fn with_format(bytes: &[u8], format: u32) -> Vec<u8> {
    let mut b = bytes.to_vec();
    b[8..12].copy_from_slice(&format.to_le_bytes());
    b
}

fn code(e: StreamError) -> (&'static str, String) {
    (e.code, e.message)
}

fn file(dir: &Path, name: &str, bytes: &[u8]) -> std::fs::File {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    std::fs::File::open(p).unwrap()
}

fn qa_sync_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rdy-qa/fixtures/sync")
        .join(name)
}

fn take_code<E: std::fmt::Display>(e: TakeError<E>) -> &'static str {
    match e {
        TakeError::Stream(e) => e.code,
        other => panic!("not a stream refusal: {other}"),
    }
}

#[test]
fn r59017_format5_is_additive_and_download_only() {
    // byte 4 is the canon state, in format 5 alone; the older purposes keep their bytes
    assert_eq!(CANON_STATE_FORMAT, 5);
    assert_eq!(HISTORY_FORMAT, 4);
    assert_eq!(READ_FORMATS, [2, 3, 4, 5]);
    let state_purpose = StreamPurpose::CanonState;
    assert_eq!(StreamPurpose::of(4), Some(state_purpose));
    assert_eq!(StreamPurpose::of(5), None);
    assert_eq!(state_purpose.byte(), 4);
    assert_eq!(state_purpose.name(), "canon_state");
    assert_eq!(state_purpose.first_format(), 5);
    assert_eq!(state_purpose.root_kind(), kinds::LANDING);
    assert!(!state_purpose.is_history());
    for (byte, purpose, history) in [
        (0, StreamPurpose::TagHistory, true),
        (1, StreamPurpose::LinkDelivery, false),
        (2, StreamPurpose::CanonHistory, true),
        (3, StreamPurpose::ViewHistory, true),
    ] {
        assert_eq!(StreamPurpose::of(byte), Some(purpose));
        assert_eq!(purpose.is_history(), history);
        assert!(!purpose.in_format(5), "{purpose:?} is not in format 5");
        assert!(purpose.in_format(4) == (purpose.first_format() <= 4));
    }
    for format in [2, 3, 4] {
        assert!(!state_purpose.in_format(format));
    }
    assert!(state_purpose.in_format(5));

    // the frozen format-4 fixture keeps its bytes
    let sync4 = std::fs::read(qa_sync_fixture("sync4-canon-history.bin")).unwrap();
    assert_eq!(
        (sync4.len(), Hash::of(&sync4).to_string()),
        (
            5884,
            "8d51365e1d9bd84bf9c00526e672d401298d0128cdf3590d1c09f50eb5f746e4".to_string()
        )
    );
    assert_eq!(check_batch(&sync4[..]).unwrap().header.stream_format, 4);

    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("p");
    let mut s = Store::create(&path).unwrap();
    let l1 = land(&mut s, 1);
    let l2 = land(&mut s, 2);
    let l3 = land(&mut s, 3);
    let reader = s.reader();
    let (bytes, header, sel) = state(&reader, l3);
    assert_eq!(&bytes[8..12], &5u32.to_le_bytes());
    assert_eq!(header.stream_format, 5);
    assert_eq!(header.purpose, StreamPurpose::CanonState);
    assert_eq!(
        header.root(),
        ObjectKey {
            kind: kinds::LANDING,
            hash: l3
        }
    );
    assert!(header.features.has(FEATURE_PARTIAL_HISTORY));
    // the state holds the tip, not its history: the earlier landings are frontier edges
    assert!(!sel.records.iter().any(|k| k.hash == l1 || k.hash == l2));
    assert!(sel.prerequisites.contains(&ObjectKey {
        kind: kinds::LANDING,
        hash: l2
    }));
    let report = check_canon_state_batch(&bytes[..]).unwrap();
    assert_eq!(report.header, header);
    assert_eq!(report.keys, sel.records);
    // partial_history: registered, needs 0.59.0, never written by a new store; a reader that
    // does not know it refuses the header with the format refusal
    assert!(rdy_store::format::REGISTERED_FEATURES.contains(&FEATURE_PARTIAL_HISTORY));
    assert_eq!(
        rdy_store::format::required_rdy(FEATURE_PARTIAL_HISTORY),
        "0.59.0"
    );
    assert!(!rdy_store::format::baseline_features().has(FEATURE_PARTIAL_HISTORY));
    assert!(!s.features().has(FEATURE_PARTIAL_HISTORY));
    {
        let _older = rdy_store::format::older_reader_for_tests(&[FEATURE_PARTIAL_HISTORY]);
        let e = check_canon_state_batch(&bytes[..]).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_STORE_FORMAT",
                "store feature partial_history needs rdy 0.59.0",
                "open the project with rdy 0.59.0 or later"
            )
        );
    }

    // no history check, reader or older format admits it
    assert_eq!(
        code(check_batch(&bytes[..]).unwrap_err()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 4 does not match tag_history".to_string()
        )
    );
    assert_eq!(
        code(StreamReader::open_history(&bytes[..]).err().unwrap()).0,
        "E_STREAM_PURPOSE"
    );
    assert_eq!(
        code(
            StreamReader::open_for(&bytes[..], StreamPurpose::CanonHistory)
                .err()
                .unwrap()
        ),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 4 does not match canon_history".to_string()
        )
    );
    for format in [2, 3, 4] {
        assert_eq!(
            code(check_canon_state_batch(&with_format(&bytes, format)[..]).unwrap_err()).0,
            "E_STREAM_PURPOSE",
            "a canon state in format {format}"
        );
    }
    let old = StreamHeader {
        stream_format: 4,
        ..header.clone()
    };
    assert_eq!(
        StreamWriter::new(Vec::new(), &old).err().map(|e| e.code),
        Some("E_STREAM_PURPOSE")
    );
    // a history in format 5 is no canon state, and no history either: a purpose its format does
    // not know keeps the existing mapper's text; a canon history in format 4 is not a state
    let canon = canon_history(&reader, l3);
    let canon5 = with_format(&canon, 5);
    let unknown = (
        "E_STREAM_PURPOSE",
        "stream purpose 2 does not match tag_history".to_string(),
    );
    assert_eq!(code(check_batch(&canon5[..]).unwrap_err()), unknown);
    assert_eq!(
        code(check_canon_state_batch(&canon5[..]).unwrap_err()),
        unknown
    );
    assert_eq!(
        code(check_canon_state_batch(&canon[..]).unwrap_err()),
        (
            "E_STREAM_PURPOSE",
            "stream purpose 2 does not match canon_state".to_string()
        )
    );
    // a format no rdy reads stays a bad header
    assert_eq!(
        check_canon_state_batch(&with_format(&bytes, 6)[..])
            .unwrap_err()
            .code,
        "E_BAD_RECORD"
    );
    let repo = reader.repo_id().unwrap().unwrap();
    drop(reader);
    drop(s);

    // every history take refuses it at its header: a holder's push, a publication upload, a
    // view push and a fork import; a project's fetch
    let hd = tempfile::tempdir().unwrap();
    let mut h = Store::create_holder(&hd.path().join("h"), repo).unwrap();
    for purpose in [StreamPurpose::CanonHistory, StreamPurpose::ViewHistory] {
        let target = HistoryTarget { purpose, root: l3 };
        assert_eq!(
            take_code(
                h.take_history::<Infallible>(&[file(d.path(), "push", &bytes)], &target)
                    .unwrap_err()
            ),
            "E_STREAM_PURPOSE",
            "{purpose:?}"
        );
    }
    let upload = UploadTarget {
        tag: "v1".into(),
        landing: l3,
    };
    assert_eq!(
        take_code(
            h.take_batches::<Infallible>(&[file(d.path(), "up", &bytes)], &upload, |_, _| {
                Ok(rdy_store::take_batch::TagChoice::Incoming)
            })
            .unwrap_err()
        ),
        "E_STREAM_PURPOSE"
    );
    let fd = tempfile::tempdir().unwrap();
    let mut fork = Store::create_holder(&fd.path().join("f"), RepoId([3; 16])).unwrap();
    assert_eq!(
        take_code(
            fork.import_fork::<Infallible>(&[file(d.path(), "fork", &bytes)], l3, repo, &[])
                .unwrap_err()
        ),
        "E_STREAM_PURPOSE"
    );
    let pd = tempfile::tempdir().unwrap();
    let mut q = Store::create(&pd.path().join("q")).unwrap();
    let fetch = FetchTarget {
        remote: "origin".into(),
        // test-hygiene: allow port the fetch target records this URL; nothing connects to it
        url: "https://127.0.0.1:9/r/demo".into(),
        purpose: StreamPurpose::CanonHistory,
        root: l3,
        view: None,
    };
    let end = q.log_end();
    assert_eq!(
        take_code(
            q.take_fetched::<Infallible>(&[file(d.path(), "fetch", &bytes)], &fetch, |_, _| Ok(()))
                .unwrap_err()
        ),
        "E_STREAM_PURPOSE"
    );
    assert_eq!(q.log_end(), end, "a refused take writes nothing");
    drop(path);
}

/// A landing on `prev` (a copy of its record, `n` ms later) whose state is `state`: it lands
/// nothing again.
fn forged(s: &mut Store, prev: Hash, state: Hash, n: i64) -> Hash {
    let mut rec = s.reader().landing(&prev).unwrap();
    rec.read_identity = None;
    rec.prev = Some(prev);
    rec.changes = vec![];
    rec.state = state;
    rec.approved_by.at_ms += n;
    s.put(kinds::LANDING, &rec).unwrap()
}

/// SNAPSHOT bytes of the two-entry map `a`, `b` (ascending) with the keys reordered: they decode
/// to the map but are not its canonical encoding.
fn reordered(a: (&BlockId, Hash), b: (&BlockId, Hash)) -> Vec<u8> {
    let entry = |id: &BlockId, v: Hash| {
        let s = id.to_string();
        let mut e = vec![s.len() as u8];
        e.extend(s.as_bytes());
        e.extend(v.0);
        e
    };
    let mut out = vec![2u8];
    out.extend(entry(b.0, b.1));
    out.extend(entry(a.0, a.1));
    out
}

/// The refusal of the state of `root`, with the store's log end checked unchanged.
fn refused(s: &Store, root: Hash) -> (&'static str, String) {
    let end = s.log_end();
    let e = select_state(&s.reader(), root, END_OF_LOG).unwrap_err();
    assert_eq!(s.log_end(), end, "a refused selection writes nothing");
    (e.code, e.message)
}

#[test]
fn r59017_state_snapshot_matches_the_landing() {
    // held SNAPSHOTs (a store before snapshot deltas): the map is the SNAPSHOT the state names
    {
        let _legacy = rdy_store::format::legacy_writer_for_tests();
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::create(&d.path().join("p")).unwrap();
        land(&mut s, 1);
        let l2 = land(&mut s, 2);
        let map = s.reader().snapshot_at(&l2).unwrap().blocks;
        let mut it = map.iter();
        let (a, va) = it.next().unwrap();
        let (b, vb) = it.next().unwrap();
        assert!(select_state(&s.reader(), l2, END_OF_LOG).is_ok());

        // bytes of the map that are not canonical
        let state = s
            .write(vec![(kinds::SNAPSHOT, reordered((a, *va), (b, *vb)))])
            .unwrap()[0];
        let l = forged(&mut s, l2, state, 1);
        let (code, message) = refused(&s, l);
        assert_eq!(code, "E_BAD_RECORD");
        assert!(
            message.contains(&format!("{state}")) && message.contains("canonical"),
            "{message}"
        );
        // a state that names an object of another kind
        let l = forged(&mut s, l2, *va, 2);
        let (code, message) = refused(&s, l);
        assert_eq!(
            (code, message.as_str()),
            (
                "E_BAD_RECORD",
                "stream record 0 at byte 0: record does not decode"
            )
        );
        // a state no store holds
        let l = forged(&mut s, l2, Hash([0x5A; 32]), 8);
        assert_eq!(refused(&s, l).0, "E_UNRESOLVED_REF");
        // a map that names another block's version under an id
        let wrong = StateSnapshot {
            blocks: [(a.clone(), *vb), (b.clone(), *vb)].into_iter().collect(),
        };
        let state = s.put(kinds::SNAPSHOT, &wrong).unwrap();
        let l = forged(&mut s, l2, state, 3);
        assert_eq!(
            refused(&s, l),
            (
                "E_BAD_RECORD",
                format!("snapshot {state}: block {a} names version {vb} of another block")
            )
        );
        // a map that names a current version no store holds
        let gone = Hash([0x6B; 32]);
        let absent = StateSnapshot {
            blocks: [(a.clone(), *va), (b.clone(), gone)].into_iter().collect(),
        };
        let state = s.put(kinds::SNAPSHOT, &absent).unwrap();
        let l = forged(&mut s, l2, state, 4);
        let (code, message) = refused(&s, l);
        assert_eq!(code, "E_UNRESOLVED_REF");
        assert!(message.contains(&gone.to_string()), "{message}");
    }

    // snapshot deltas: the tip's delta must be there and name the landing and its map
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::create(&d.path().join("p")).unwrap();
    assert!(s.features().has(rdy_store::format::FEATURE_SNAPSHOT_DELTA));
    land(&mut s, 1);
    let l2 = land(&mut s, 2);
    let sel = select_state(&s.reader(), l2, END_OF_LOG).unwrap();
    let deltas: Vec<&ObjectKey> = sel
        .records
        .iter()
        .filter(|k| k.kind == kinds::SNAPSHOT_DELTA)
        .collect();
    let (base, _) = s.reader().snapshot_delta_of(&l2).unwrap().unwrap();
    assert_eq!(
        deltas,
        [&ObjectKey {
            kind: kinds::SNAPSHOT_DELTA,
            hash: base
        }]
    );
    let state = s.reader().landing(&l2).unwrap().state;
    let map = s.reader().snapshot_at(&l2).unwrap().blocks;
    // a tip with no delta
    let l = forged(&mut s, l2, state, 5);
    let (code, message) = refused(&s, l);
    assert_eq!(code, "E_BAD_RECORD");
    assert_eq!(
        message,
        format!("landing {l}: expected exactly one snapshot delta")
    );
    // a delta of the tip whose full hash is not the landing's state
    let l = forged(&mut s, l2, state, 6);
    let mut delta = diff_snapshots(l, Some(base), &map, &map);
    delta.full = Hash([0x7C; 32]);
    let dh = s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
    let (code, message) = refused(&s, l);
    assert_eq!(code, "E_BAD_RECORD");
    assert_eq!(
        message,
        format!("snapshot delta {dh}: landing or full hash does not match")
    );
    // a delta whose set does not fold to the map
    let l = forged(&mut s, l2, state, 7);
    let mut delta = diff_snapshots(l, Some(base), &map, &map);
    let (id, _) = map.iter().next().unwrap();
    delta.set.push((id.clone(), Hash([0x7D; 32])));
    let dh = s.put(kinds::SNAPSHOT_DELTA, &delta).unwrap();
    let (code, message) = refused(&s, l);
    assert_eq!(code, "E_BAD_RECORD");
    assert!(
        message.starts_with(&format!("snapshot delta {dh}: ")),
        "{message}"
    );
}
