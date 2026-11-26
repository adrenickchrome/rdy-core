//! r5907 (D-3PNRK, D-H0BZN): a canon or view history's prepared closure keeps what its walk
//! found of each emitted record (its order, its resolved links, its removal proof and its frame
//! length), so a sender plans its batches without reading the records again; and the walk reads
//! the index in one read transaction.
use crate::carried_fixture;
use crate::redaction_fixture;

use carried_fixture::{append_raw, find, framed_at, poke};
use rdy_model::redaction::RedactTarget;
use rdy_model::*;
use rdy_store::closure::{
    END_OF_LOG, PreparedClosure, StoreSource, history_closure_before,
    prepared_history_closure_before, write_batch,
};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{StreamHeader, StreamPurpose, sync_kinds};
use rdy_store::{Store, StoreReader, ViewOp, ViewOpKind};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, enc};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The bytes of a stream frame's header: length, kind and hash.
const FRAME_HEADER: u64 = 37;

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

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

fn block(n: u32, body: &str) -> Block {
    Block {
        id: BlockId::parse(&format!("b-{n:05}")).unwrap(),
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

fn change(n: u32, body: &str) -> Change {
    Change {
        view: "work".into(),
        ops: vec![Op::Create {
            block: block(n, body),
        }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: format!("change {n}"),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// Commits `c` to view `work` and lands it: the landing.
fn land(s: &mut Store, c: Change, name: &str) -> Hash {
    s.commit_to_view("work", c).unwrap();
    let p = s.propose("work", name, prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap()
}

/// A project with three landings, the second a port (D-HX6BX), and an open view `draft` that
/// holds one more change: the store's path and its landings.
fn project() -> (tempfile::TempDir, PathBuf, [Hash; 3]) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("p");
    let mut s = Store::create(&path).unwrap();
    let a = land(&mut s, change(1, "first"), "p1");
    let mut ported = change(2, "ported text");
    ported.links.ported_from = Some(PortedFrom {
        repo_id: RepoId([9; 16]),
        change: Hash([77; 32]),
    });
    let b = land(&mut s, ported, "p2");
    let c = land(&mut s, change(3, "third"), "p3");
    let mut draft = change(4, "drafted");
    draft.view = "draft".into();
    s.commit_to_view("draft", draft).unwrap();
    (d, path, [a, b, c])
}

/// A project whose first landing's block a later landing removed: the store's path and the
/// landing that applies the removal.
fn removed() -> (tempfile::TempDir, PathBuf, Hash) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("r");
    let mut s = Store::create(&path).unwrap();
    let secret = block(1, "canary alpha secret");
    let mut c = change(1, "");
    c.ops = vec![Op::Create {
        block: secret.clone(),
    }];
    land(&mut s, c, "p1");
    drop(s);
    let spec = RequestSpec::new(
        "work",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
    );
    let fixture = authorized_batch_fixture(
        &path,
        &[spec],
        &FixtureOptions::approved(Proof::local_human()),
    );
    (d, path, fixture.landing.unwrap())
}

/// The prepared closure of `purpose` from `root`, checked against what a reader of the encoded
/// history finds: the same closure as [`history_closure_before`]; per record, in order, its key,
/// its sorted resolved links, its proof and its frame's bytes; and the frames' bytes sum to the
/// written batch's body.
fn checked(
    reader: &StoreReader,
    purpose: StreamPurpose,
    root: Hash,
    known: &BTreeSet<ObjectKey>,
    base: Option<Hash>,
) -> PreparedClosure {
    let p =
        prepared_history_closure_before(reader, purpose, root, known, base, END_OF_LOG).unwrap();
    assert_eq!(
        p.history,
        history_closure_before(reader, purpose, root, known, base, END_OF_LOG).unwrap()
    );
    assert_eq!(p.records.len(), p.history.records.len());
    let features = reader.store_features().unwrap();
    let mut src = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    for (r, k) in p.records.iter().zip(&p.history.records) {
        assert_eq!(r.key, *k, "the records keep the emitted order");
        let record = src.record(*k).unwrap().unwrap();
        assert_eq!(r.frame_len, FRAME_HEADER + record.frame_len(), "{k:?}");
        assert_eq!(r.proof, record.proof(), "{k:?}");
        let mut refs: Vec<ObjectKey> = record
            .refs(&features)
            .unwrap()
            .into_iter()
            .map(|x| match x.kind {
                Some(kind) => key(kind, x.hash),
                None => {
                    let held: Vec<u8> = sync_kinds()
                        .iter()
                        .copied()
                        .filter(|kind| src.holds(*kind, &x.hash).unwrap())
                        .collect();
                    assert_eq!(held.len(), 1, "one held record of a kind-none link");
                    key(held[0], x.hash)
                }
            })
            .collect();
        refs.sort_unstable();
        refs.dedup();
        assert_eq!(r.refs, refs, "{k:?}");
    }
    // the frames' bytes are the batch's body: the full batch less the batch of no record
    let repo = reader.repo_id().unwrap().unwrap();
    let header = |prerequisites: Vec<ObjectKey>| match purpose {
        StreamPurpose::ViewHistory => {
            StreamHeader::view_history(p.history.features.clone(), repo, root, prerequisites)
        }
        _ => StreamHeader::canon_history(p.history.features.clone(), repo, root, prerequisites),
    };
    let write = |records: &[ObjectKey]| {
        let mut src = StoreSource {
            reader,
            end: END_OF_LOG,
        };
        let h = header(p.history.prerequisites.clone());
        write_batch(&mut src, &h, records, Vec::new()).unwrap().0
    };
    let full = write(&p.history.records).len() as u64;
    let empty = write(&[]).len() as u64;
    assert_eq!(
        full - empty,
        p.records.iter().map(|r| r.frame_len).sum::<u64>()
    );
    p
}

#[test]
fn r5907_metadata_matches_encoded_history() {
    let (_d, path, [a, b, c]) = project();
    let s = Store::open(&path).unwrap();
    let reader = s.reader();
    let none = BTreeSet::new();
    // canon, nothing known: the ported change is a record, and its port is no link
    let full = checked(&reader, StreamPurpose::CanonHistory, c, &none, None);
    let ported = full
        .records
        .iter()
        .filter(|r| r.key.kind == kinds::CHANGE)
        .count();
    assert_eq!(ported, 3, "three changes, one of them a port");
    assert!(full.records.iter().any(|r| r.key == key(kinds::LANDING, b)));
    // canon above a base, and with part of the history known
    let above = checked(&reader, StreamPurpose::CanonHistory, c, &none, Some(a));
    assert!(above.records.len() < full.records.len());
    let half: BTreeSet<ObjectKey> = full.history.records[..full.records.len() / 2]
        .iter()
        .copied()
        .collect();
    let partial = checked(&reader, StreamPurpose::CanonHistory, c, &half, None);
    assert!(partial.records.iter().all(|r| !half.contains(&r.key)));
    // a view history
    let head = reader.view_head("draft").unwrap().unwrap().head;
    let view = checked(&reader, StreamPurpose::ViewHistory, head, &none, None);
    assert!(
        view.records
            .iter()
            .any(|r| r.key == key(kinds::VIEW_OP, head))
    );

    // a removed block: its placeholder keeps its proof and its residue link
    let (_d, path, landing) = removed();
    let s = Store::open(&path).unwrap();
    let reader = s.reader();
    let p = checked(&reader, StreamPurpose::CanonHistory, landing, &none, None);
    let placeholder = p
        .records
        .iter()
        .find(|r| r.proof.is_some())
        .expect("one removed record");
    assert!(
        placeholder
            .refs
            .iter()
            .any(|k| k.kind == kinds::REDACTION_RESIDUE),
        "the placeholder links its residue"
    );
    assert_eq!(placeholder.proof.unwrap().kind, kinds::REDACTION_APPLY);
}

/// The refusal of the history of `purpose` from `root` of the store at `path`, by code and
/// message, after `damage` ran on the open store's files.
fn refusal(
    path: &Path,
    purpose: StreamPurpose,
    root: Hash,
    damage: impl FnOnce(),
) -> (String, String) {
    let s = Store::open(path).unwrap();
    damage();
    let reader = s.reader();
    let e: StreamError =
        prepared_history_closure_before(&reader, purpose, root, &BTreeSet::new(), None, END_OF_LOG)
            .unwrap_err();
    // the closure alone refuses the same way
    let again = history_closure_before(&reader, purpose, root, &BTreeSet::new(), None, END_OF_LOG)
        .unwrap_err();
    assert_eq!((e.code, &e.message), (again.code, &again.message));
    (e.code.to_string(), e.message)
}

/// Overwrites the length of the frame at `at` of the store at `dir` with one past any record.
fn damage_header(dir: &Path, at: rdy_store::segment::Location) {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(
            dir.join("segments")
                .join(format!("seg-{:06}.log", at.segment)),
        )
        .unwrap();
    file.seek(SeekFrom::Start(at.offset)).unwrap();
    file.write_all(&[0xff; 4]).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn r5907_one_transaction_and_no_second_metadata_read() {
    let (_d, path, [_, _, c]) = project();
    let s = Store::open(&path).unwrap();
    let reader = s.reader();
    let none = BTreeSet::new();
    let (txns, frames) = (reader.index_read_transactions(), reader.frames_read());
    let p = prepared_history_closure_before(
        &reader,
        StreamPurpose::CanonHistory,
        c,
        &none,
        None,
        END_OF_LOG,
    )
    .unwrap();
    assert_eq!(
        reader.index_read_transactions() - txns,
        1,
        "the whole walk reads the index in one transaction"
    );
    // each record of the walk is read once, and no attachment pass reads it again
    let walked = reader.frames_read() - frames;
    assert!(walked >= p.records.len() as u64);
    let (txns, frames) = (reader.index_read_transactions(), reader.frames_read());
    let again = prepared_history_closure_before(
        &reader,
        StreamPurpose::CanonHistory,
        c,
        &none,
        Some(c),
        END_OF_LOG,
    )
    .unwrap();
    assert_eq!(
        reader.index_read_transactions() - txns,
        1,
        "a base walk too"
    );
    assert!(again.records.len() < p.records.len());
    assert!(reader.frames_read() - frames <= 2 * walked);
    drop(s);

    // a view op whose change the store does not hold: its original refusal
    let (_d, path, _) = project();
    let ghost = ViewOp {
        view: "ghost".into(),
        prev: None,
        op: ViewOpKind::Commit {
            change: Hash([7; 32]),
        },
        actor: "agent:w".into(),
        run: None,
        at_ms: 5,
    };
    let frame = enc(&ghost);
    let op = Hash::of(&frame);
    append_raw(&path, &[(kinds::VIEW_OP, frame)], true);
    assert_eq!(
        refusal(&path, StreamPurpose::ViewHistory, op, || ()),
        (
            "E_UNRESOLVED_REF".to_string(),
            format!("stream record 0 needs 2:{}", Hash([7; 32]))
        )
    );

    // a landing whose frame header was damaged: the frame the index places is no frame
    let (_d, path, [a, _, c]) = project();
    let frames = framed_at(&path);
    let f = find(&frames, kinds::LANDING, a);
    let damaged = refusal(&path, StreamPurpose::CanonHistory, c, || {
        damage_header(&path, f.loc)
    });
    let frame = |at| format!("stream input: corrupt or missing frame at {at:?}");
    assert_eq!(damaged, ("E_IO".to_string(), frame(f.loc)));

    // a change whose payload no longer matches its hash: a corrupt payload
    let (_d, path, [a, _, c]) = project();
    let s = Store::open(&path).unwrap();
    let landing: LandingRecord = s.reader().get_typed(kinds::LANDING, &a).unwrap();
    drop(s);
    let frames = framed_at(&path);
    let f = find(&frames, kinds::CHANGE, landing.changes[0]);
    let corrupt = refusal(&path, StreamPurpose::CanonHistory, c, || {
        poke(&path, &f, f.len / 2, 0xa5)
    });
    assert_eq!(corrupt, ("E_IO".to_string(), frame(f.loc)));
}
