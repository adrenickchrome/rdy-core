//! The take of an upload into a holder (plan HA1 Task 11; D-BPYN8: closure and tag in one batch;
//! D-D2NPN: verify on receive; hub p-97JVE, f-NE0Z4; O11): every batch file of one upload makes
//! one sealed batch, whatever their order or repeats; the root must name the publication target;
//! nothing is written on a refusal; changed input rolls back; a repeat is a no-op; the relay-tag
//! answer ref and SIG's subject rule hold at the take.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::holder::answer_tag_object_ref;
use rdy_store::index::IndexEntry;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{StreamHeader, StreamReader, StreamRecord};
use rdy_store::take_batch::{
    BatchTaken, HolderCapacity, TagChoice, TakeError, UploadFacts, UploadTarget,
    check_input_bounds, set_holder_capacity_for_tests,
};
use rdy_store::{Store, StoreReader};
use redaction_fixture::{ORIGIN, enc, segment_bytes};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
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
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn answer(item: &str, targets: Vec<Hash>, note: Option<&str>) -> Answer {
    Answer {
        item: item.into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: prov("human:alice", Role::Approver),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: note.map(String::from),
    }
}

fn sig(subject: Hash, namespace: &str, n: u8) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: format!("statement {n}").into_bytes(),
        sig: vec![n],
        signed_by: prov("human:alice", Role::Approver),
    }
}

/// A source project: one change of two blocks, landed, tagged `v1` with its relay answer
/// (`answer/tag:v1` in the source, O11), with a signature of the landing and one of the tag.
struct Src {
    dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    landing: Hash,
    tag: Hash,
    relay: Hash,
    alpha: Block,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let alpha = block("b-00001", "alpha");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![
                Op::Create {
                    block: alpha.clone(),
                },
                Op::Create {
                    block: block("b-00002", "beta"),
                },
            ],
        ),
    )
    .unwrap();
    let proposal = s
        .propose("work", "first", prov("agent:w", Role::Writer))
        .unwrap();
    let landing = s
        .land(&proposal, prov("human:alice", Role::Approver))
        .unwrap();
    let (tag, relay) = s
        .put_tag_answered(
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
            answer("tag:v1", vec![], None),
        )
        .unwrap();
    s.put_signature(&sig(landing, "rdy-approval-v1", 1))
        .unwrap();
    s.put_signature(&sig(tag, "rdy-tag-v1", 2)).unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    drop(s);
    Src {
        dir,
        path,
        repo,
        landing,
        tag,
        relay,
        alpha,
    }
}

impl Src {
    fn reader(&self) -> (Store, StoreReader) {
        let s = Store::open(&self.path).unwrap();
        let r = s.reader();
        (s, r)
    }

    fn target(&self) -> UploadTarget {
        UploadTarget {
            tag: "v1".into(),
            landing: self.landing,
        }
    }

    /// The whole closure of `tag`.
    fn closure(&self, tag: Hash) -> Vec<ObjectKey> {
        let (_s, r) = self.reader();
        closure(&r, tag, &BTreeSet::new()).unwrap().records
    }

    /// One batch file per part of `tag`'s closure: each part's records (in dependency order)
    /// with its exact prerequisites, as the closure gives them when the rest is known.
    fn batches(&self, tag: Hash, parts: &[BTreeSet<ObjectKey>], name: &str) -> Vec<PathBuf> {
        let (_s, r) = self.reader();
        let all: BTreeSet<ObjectKey> = closure(&r, tag, &BTreeSet::new())
            .unwrap()
            .records
            .into_iter()
            .collect();
        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let known: BTreeSet<ObjectKey> = all.difference(part).copied().collect();
                let c = closure(&r, tag, &known).unwrap();
                assert_eq!(c.records.iter().copied().collect::<BTreeSet<_>>(), *part);
                let path = self.dir.path().join(format!("{name}-{i}.rdysync"));
                write(&r, &self.repo, tag, &c.records, c.prerequisites, &path);
                path
            })
            .collect()
    }

    /// The closure of `tag` cut into `n` contiguous parts.
    fn split(&self, tag: Hash, n: usize, name: &str) -> Vec<PathBuf> {
        let records = self.closure(tag);
        let size = records.len().div_ceil(n);
        let parts: Vec<BTreeSet<ObjectKey>> = records
            .chunks(size)
            .map(|c| c.iter().copied().collect())
            .collect();
        self.batches(tag, &parts, name)
    }
}

fn write(
    r: &StoreReader,
    repo: &RepoId,
    tag: Hash,
    records: &[ObjectKey],
    prerequisites: Vec<ObjectKey>,
    path: &Path,
) {
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        *repo,
        tag,
        prerequisites,
    );
    let mut source = StoreSource {
        reader: r,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, records, File::create(path).unwrap()).unwrap();
}

/// A batch of the records `extra` (kind and bytes, in that order) with `prerequisites`, rooted
/// at `tag`.
fn write_extra(
    repo: &RepoId,
    tag: Hash,
    extra: &[(u8, Vec<u8>)],
    prerequisites: Vec<ObjectKey>,
    path: &Path,
) {
    let mut keys = vec![];
    let mut source: BTreeMap<ObjectKey, StreamRecord> = BTreeMap::new();
    for (kind, payload) in extra {
        let k = key(*kind, Hash::of(payload));
        keys.push(k);
        source.insert(
            k,
            StreamRecord::Present {
                encoding: 0,
                key: k,
                payload: payload.clone(),
            },
        );
    }
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        *repo,
        tag,
        prerequisites,
    );
    write_batch(&mut source, &header, &keys, File::create(path).unwrap()).unwrap();
}

fn open(paths: &[PathBuf]) -> Vec<File> {
    paths.iter().map(|p| File::open(p).unwrap()).collect()
}

fn holder(src: &Src, name: &str) -> (PathBuf, Store) {
    let path = src.dir.path().join(name);
    let h = Store::create_holder(&path, src.repo).unwrap();
    (path, h)
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

fn take(
    h: &mut Store,
    files: &[File],
    target: &UploadTarget,
) -> Result<BatchTaken, TakeError<StreamError>> {
    h.take_batches(files, target, incoming)
}

/// Every frame of the log as (location, kind, hash, dropped).
fn log_frames(h: &Store) -> Vec<(rdy_store::segment::Location, u8, Hash, bool)> {
    let mut out = vec![];
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |loc, f, _| {
            out.push(match f {
                StoredFrame::Present { kind, hash, .. } => (loc, kind, hash, false),
                StoredFrame::Dropped { kind, hash, .. } => (loc, kind, hash, true),
            });
            Ok(())
        })
        .unwrap();
    out
}

fn markers(h: &Store) -> usize {
    log_frames(h)
        .iter()
        .filter(|(_, k, _, _)| *k == kinds::BATCH_COMMIT)
        .count()
}

fn stream_err(e: TakeError<StreamError>) -> StreamError {
    match e {
        TakeError::Stream(e) => e,
        other => panic!("not a stream refusal: {other:?}"),
    }
}

#[test]
fn ha11_many_files_make_one_sealed_take() {
    let src = source();
    let records = src.closure(src.tag);
    let paths = src.split(src.tag, 3, "three");
    assert_eq!(paths.len(), 3);
    let (_, mut h) = holder(&src, "h");
    let before = markers(&h);
    let end = h.log_end();
    let got = take(&mut h, &open(&paths), &src.target()).unwrap();
    // one marker over every record and the two refs
    assert_eq!(markers(&h), before + 1);
    assert_eq!(got.new_objects, records.len() as u64);
    assert_eq!(got.frames, records.len() as u64 + 2);
    assert_eq!(got.redacted_objects, 0);
    assert_eq!(got.end, h.log_end());
    assert_eq!(got.tag_hash, src.tag);
    assert_eq!(got.tag.name, "v1");
    let data: Vec<_> = log_frames(&h)
        .into_iter()
        .filter(|(l, k, _, _)| *l >= end && *k != kinds::BATCH_COMMIT)
        .collect();
    assert_eq!(data.len() as u64, got.frames);
    for k in &records {
        assert!(h.has_kind(k.kind, &k.hash).unwrap(), "{k} is held");
    }
    // the records in the closure's dependency order, then the tag ref and the relay answer ref,
    // named by the tag object in a holder (ruling task-13)
    let written: Vec<ObjectKey> = data
        .iter()
        .filter(|(_, k, _, _)| *k != kinds::REF_UPDATE)
        .map(|(_, k, h, _)| key(rdy_store::codec::logical_kind(*k), *h))
        .collect();
    assert_eq!(written, records);
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    assert_eq!(h.get_ref("answer/tag:v1").unwrap(), None);
    h.check_meta().unwrap();
    // the sealed take reopens
    let path = h.dir().to_path_buf();
    drop(h);
    let h = Store::open(&path).unwrap();
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
}

#[test]
fn ha11_batch_order_and_repeats_do_not_change_the_take() {
    let src = source();
    let p = src.split(src.tag, 3, "three");
    let (a_path, mut a) = holder(&src, "a");
    let (b_path, mut b) = holder(&src, "b");
    let in_order = take(&mut a, &open(&p), &src.target()).unwrap();
    let shuffled = [
        p[2].clone(),
        p[0].clone(),
        p[1].clone(),
        p[0].clone(),
        p[2].clone(),
    ];
    let repeated = take(&mut b, &open(&shuffled), &src.target()).unwrap();
    assert_eq!(in_order, repeated);
    drop((a, b));
    assert_eq!(segment_bytes(&a_path), segment_bytes(&b_path));
    // one whole batch and its parts make the same take too
    let whole = src.split(src.tag, 1, "one");
    let (c_path, mut c) = holder(&src, "c");
    let mixed = [p[1].clone(), whole[0].clone(), p[1].clone()];
    take(&mut c, &open(&mixed), &src.target()).unwrap();
    drop(c);
    assert_eq!(segment_bytes(&a_path), segment_bytes(&c_path));
}

#[test]
fn ha11_publication_target_mismatch_writes_nothing() {
    let src = source();
    let paths = src.split(src.tag, 2, "two");
    let (path, mut h) = holder(&src, "h");
    let before = segment_bytes(&path);
    let wrong = [
        // the exact name, byte for byte: no case fold, no trim, no optional `v`
        UploadTarget {
            tag: "V1".into(),
            landing: src.landing,
        },
        UploadTarget {
            tag: "v1 ".into(),
            landing: src.landing,
        },
        UploadTarget {
            tag: "1".into(),
            landing: src.landing,
        },
        // the right name at another landing
        UploadTarget {
            tag: "v1".into(),
            landing: src.tag,
        },
    ];
    for t in &wrong {
        let e = stream_err(take(&mut h, &open(&paths), t).unwrap_err());
        assert_eq!(e.code, "E_BAD_RECORD");
        assert_eq!(e.message, "upload target does not match the batch root tag");
        assert_eq!(
            e.fix,
            "upload the history for the publication tag and landing"
        );
        assert_eq!(e.object, Some(key(kinds::TAG, src.tag)));
    }
    // the target names no hash: another TAG named v1 is refused at another landing too
    let mut s = Store::open(&src.path).unwrap();
    let other = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: src.landing,
            message: Some("another v1".into()),
            tagged_by: prov("human:alice", Role::Approver),
        })
        .unwrap();
    drop(s);
    let other_paths = src.split(other, 1, "other");
    let e = stream_err(
        take(
            &mut h,
            &open(&other_paths),
            &UploadTarget {
                tag: "v1".into(),
                landing: src.tag,
            },
        )
        .unwrap_err(),
    );
    assert_eq!(e.message, "upload target does not match the batch root tag");
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before);
    // the bounds of the inputs, before anything is read
    let e = check_input_bounds(0, &src.target()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_RECORD", "stream record 0 at byte 0: no batch files")
    );
    let e = check_input_bounds(1025, &src.target()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "stream exceeds 1024 batch files",
            "reduce the stream to the stated limit"
        )
    );
    check_input_bounds(1024, &src.target()).unwrap();
    let long = UploadTarget {
        tag: "v".repeat(1_048_577),
        landing: src.landing,
    };
    let e = check_input_bounds(1, &long).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "upload tag exceeds 1048576 bytes",
            "reduce the tag name to the stated limit"
        )
    );
    check_input_bounds(
        1,
        &UploadTarget {
            tag: "v".repeat(1_048_576),
            landing: src.landing,
        },
    )
    .unwrap();
    // the target is a strict input
    let json = format!(r#"{{"tag":"v1","landing":"{}"}}"#, src.landing);
    let t: UploadTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(t, src.target());
    let json = format!(r#"{{"tag":"v1","landing":"{}","tga":"x"}}"#, src.landing);
    assert!(serde_json::from_str::<UploadTarget>(&json).is_err());
}

#[test]
fn ha11_missing_root_or_dependency_changes_nothing() {
    let src = source();
    let root = key(kinds::TAG, src.tag);
    let landing = key(kinds::LANDING, src.landing);
    let all: BTreeSet<ObjectKey> = src.closure(src.tag).into_iter().collect();
    let only = |k: ObjectKey| BTreeSet::from([k]);
    let rest = |out: &[ObjectKey]| -> BTreeSet<ObjectKey> {
        all.iter().filter(|k| !out.contains(k)).copied().collect()
    };
    let parts = src.batches(
        src.tag,
        &[only(root), only(landing), rest(&[root, landing])],
        "cut",
    );
    let (path, mut h) = holder(&src, "h");
    let before = (
        segment_bytes(&path),
        h.refs_with_prefix("").unwrap(),
        h.log_end(),
    );
    let refused = |h: &mut Store, files: &[PathBuf]| {
        stream_err(take(h, &open(files), &src.target()).unwrap_err())
    };
    // the root missing: every other file names it
    let e = refused(&mut h, &[parts[1].clone(), parts[2].clone()]);
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    assert_eq!(e.message, format!("stream prerequisite {root} is missing"));
    assert_eq!(e.fix, "send the missing object with its dependencies");
    assert_eq!(e.object, Some(root));
    // a dependency missing: the smallest missing key of the union is named
    let e = refused(&mut h, &[parts[0].clone(), parts[2].clone()]);
    assert_eq!(
        e.message,
        format!("stream prerequisite {landing} is missing")
    );
    let e = refused(&mut h, &[parts[2].clone()]);
    assert_eq!(
        e.message,
        format!("stream prerequisite {landing} is missing")
    );
    // a batch of another repo is refused before its records count
    let other = Store::create(&src.dir.path().join("other")).unwrap();
    let other_repo = other.repo_id().unwrap().unwrap();
    drop(other);
    let foreign = src.dir.path().join("foreign.rdysync");
    {
        let (_s, r) = src.reader();
        let c = closure(&r, src.tag, &BTreeSet::new()).unwrap();
        write(
            &r,
            &other_repo,
            src.tag,
            &c.records,
            c.prerequisites,
            &foreign,
        );
    }
    match take(&mut h, &open(std::slice::from_ref(&foreign)), &src.target()).unwrap_err() {
        TakeError::RepoMismatch { source, target } => {
            assert_eq!((source, target), (other_repo, src.repo));
        }
        other => panic!("{other:?}"),
    }
    // two files of different headers
    let e = refused(&mut h, &[parts[0].clone(), foreign]);
    assert_eq!(
        e.message,
        "stream record 0 at byte 12: batch header differs"
    );
    assert_eq!(
        (
            segment_bytes(&path),
            h.refs_with_prefix("").unwrap(),
            h.log_end()
        ),
        before
    );
    // and with every part, the take passes
    take(&mut h, &open(&parts), &src.target()).unwrap();
    // a project refuses the take with its structured variant
    let mut p = Store::open(&src.path).unwrap();
    assert!(matches!(
        p.take_batches(&open(&parts), &src.target(), incoming),
        Err(TakeError::ModeMismatch)
    ));
}

#[test]
fn ha11_changed_input_during_append_rolls_back() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    // the last record of the file: its place and a byte inside its payload
    let (n, offset) = {
        let (mut r, _) = StreamReader::open(File::open(&paths[0]).unwrap()).unwrap();
        while r.next().unwrap().is_some() {}
        r.position()
    };
    let (path, h) = holder(&src, "h");
    let before = (segment_bytes(&path), h.log_end());
    let files = open(&paths);
    let barrier = Arc::new(Barrier::new(2));
    let mut h = h;
    let b = Arc::clone(&barrier);
    let last = src.closure(src.tag).len() as u64 - 1;
    h.on_take_append_for_tests(move |appended| {
        if appended == last {
            b.wait();
            b.wait();
        }
    });
    let target = src.target();
    let worker = std::thread::spawn(move || {
        let got = take(&mut h, &files, &target);
        (h, got)
    });
    // the take has appended every record but the last; change that one's bytes
    barrier.wait();
    let mut bytes = std::fs::read(&paths[0]).unwrap();
    let at = offset as usize + 37;
    bytes[at] ^= 0xFF;
    std::fs::write(&paths[0], &bytes).unwrap();
    barrier.wait();
    let (h, got) = worker.join().unwrap();
    let e = stream_err(got.unwrap_err());
    assert_eq!(e.code, "E_BAD_RECORD");
    assert_eq!(
        e.message,
        format!("stream record {n} at byte {offset}: input changed during take")
    );
    assert_eq!(h.log_end(), before.1);
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before.0);
    // put the bytes back: a path that names another file now cannot redirect an opened handle
    bytes[at] ^= 0xFF;
    std::fs::write(&paths[0], &bytes).unwrap();
    let mut h = Store::open(&path).unwrap();
    let files = open(&paths);
    let moved = src.dir.path().join("moved.rdysync");
    let p0 = paths[0].clone();
    let barrier = Arc::new(Barrier::new(2));
    let b = Arc::clone(&barrier);
    h.on_take_append_for_tests(move |appended| {
        if appended == 1 {
            b.wait();
            b.wait();
        }
    });
    let target = src.target();
    let worker = std::thread::spawn(move || {
        let got = take(&mut h, &files, &target);
        (h, got)
    });
    barrier.wait();
    std::fs::rename(&p0, &moved).unwrap();
    std::fs::write(&p0, b"RDYSYNC\nnot the batch").unwrap();
    barrier.wait();
    let (h, got) = worker.join().unwrap();
    got.unwrap();
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
}

#[test]
fn ha11_repeated_take_appends_nothing() {
    let src = source();
    let paths = src.split(src.tag, 2, "two");
    let (path, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    let end = h.log_end();
    let bytes = segment_bytes(&path);
    let again = take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(
        again,
        BatchTaken {
            new_objects: 0,
            frames: 0,
            redacted_objects: 0,
            end,
            tag_hash: src.tag,
            tag: again.tag.clone(),
        }
    );
    assert_eq!(again.tag.name, "v1");
    // the existing tag chosen: nothing either
    let existing = h
        .take_batches(&open(&paths), &src.target(), |_, _| {
            Ok::<_, StreamError>(TagChoice::Existing {
                name: "v1".into(),
                hash: src.tag,
            })
        })
        .unwrap();
    assert_eq!(
        (existing.new_objects, existing.frames, existing.end),
        (0, 0, end)
    );
    // the caller's validation error passes through, and nothing is written
    let e = h
        .take_batches(&open(&paths), &src.target(), |_, facts| {
            assert_eq!(facts.root_hash, src.tag);
            assert_eq!(facts.repo_id, src.repo);
            assert!(facts.objects.windows(2).all(|w| w[0] < w[1]));
            Err::<TagChoice, _>("no")
        })
        .unwrap_err();
    assert!(matches!(e, TakeError::Validation("no")));
    drop(h);
    assert_eq!(segment_bytes(&path), bytes);
}

#[test]
fn ha11_take_writes_the_relay_tag_answer_ref_once() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    // Incoming: the tag ref and the relay answer ref of the tag object, once
    let (_, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    let again = take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(again.frames, 0);
    // Existing: another tag of the same landing keeps the held one as the version; its own tag
    // object is stored with the ref of its relay answer (ruling task-13)
    let mut s = Store::open(&src.path).unwrap();
    let (v10, relay10) = s
        .put_tag_answered(
            &Tag {
                name: "v1.0".into(),
                landing: src.landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
            answer("tag:v1.0", vec![], None),
        )
        .unwrap();
    drop(s);
    let v10_paths = src.split(v10, 1, "v10");
    let target10 = UploadTarget {
        tag: "v1.0".into(),
        landing: src.landing,
    };
    let choose = |name: &str, hash: Hash| {
        let name = name.to_string();
        move |_: &mut dyn ObjectSource, _: &UploadFacts| {
            Ok::<_, StreamError>(TagChoice::Existing { name, hash })
        }
    };
    let got = h
        .take_batches(&open(&v10_paths), &target10, choose("v1", src.tag))
        .unwrap();
    assert_eq!((got.tag_hash, got.tag.name.as_str()), (src.tag, "v1"));
    assert!(h.has_kind(kinds::TAG, &v10).unwrap());
    assert!(h.has_kind(kinds::ANSWER, &relay10).unwrap());
    assert_eq!(h.get_ref("tag/v1.0").unwrap(), None);
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&v10)).unwrap(),
        Some(relay10.0.to_vec())
    );
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    h.check_holder().unwrap();
    // an Existing choice must name a held tag of that landing by its stored name and hash
    for (name, hash) in [("v1", v10), ("v2", src.tag), ("v1.0", v10)] {
        let e = stream_err(
            h.take_batches(&open(&paths), &src.target(), choose(name, hash))
                .unwrap_err(),
        );
        assert_eq!(
            e.message, "stream record 0 at byte 0: invalid header",
            "{name}"
        );
    }
    // two relay answers of the root (each targets the root TAG, as a relay answer does):
    // refused, whichever tag stays the version, as one tag object has one relay answer
    let second = enc(&answer("tag:v1", vec![src.tag], Some("again")));
    let extra = src.dir.path().join("second.rdysync");
    write_extra(
        &src.repo,
        src.tag,
        &[(kinds::ANSWER, second.clone())],
        vec![key(kinds::TAG, src.tag)],
        &extra,
    );
    let mut both = paths.clone();
    both.push(extra);
    let (_, mut fresh) = holder(&src, "fresh");
    let e = stream_err(take(&mut fresh, &open(&both), &src.target()).unwrap_err());
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(fresh.get_ref("tag/v1").unwrap(), None);
    let end = h.log_end();
    let e = stream_err(
        h.take_batches(&open(&both), &src.target(), choose("v1", src.tag))
            .unwrap_err(),
    );
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(h.log_end(), end);
    // one relay answer of the root, but not the one the holder holds for that tag object
    let only = vec![src.dir.path().join("second.rdysync")];
    let e = stream_err(take(&mut h, &open(&only), &src.target()).unwrap_err());
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(h.log_end(), end);
    assert!(!h.has_kind(kinds::ANSWER, &Hash::of(&second)).unwrap());
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
}

#[test]
fn ha11_signature_subjects_are_checked_at_take() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let root = key(kinds::TAG, src.tag);
    let alpha = key(kinds::BLOCK, src.alpha.version_hash());
    let landing = key(kinds::LANDING, src.landing);
    // a signature of a block: SIG allows only a landing or a tag
    let of_block = postcard::to_allocvec(&sig(alpha.hash, "rdy-approval-v1", 9)).unwrap();
    // a key binding: never stored
    let keybind = postcard::to_allocvec(&sig(src.landing, "rdy-keybind-v1", 8)).unwrap();
    for (bytes, prerequisites, text) in [
        (
            of_block,
            vec![alpha, root],
            format!(
                "signature subject {} is not a present landing or tag",
                alpha.hash
            ),
        ),
        (
            keybind,
            vec![landing, root],
            "key binding signatures are not stored".to_string(),
        ),
    ] {
        let extra = src.dir.path().join("sig.rdysync");
        write_extra(
            &src.repo,
            src.tag,
            &[(kinds::SIGNATURE, bytes)],
            prerequisites,
            &extra,
        );
        let mut files = paths.clone();
        files.push(extra);
        let (path, mut h) = holder(&src, &format!("h-{}", text.len()));
        let before = segment_bytes(&path);
        match take(&mut h, &open(&files), &src.target()).unwrap_err() {
            TakeError::Store(rdy_store::StoreError::Refused(m)) => assert_eq!(m, text),
            other => panic!("{other:?}"),
        }
        drop(h);
        assert_eq!(segment_bytes(&path), before);
    }
    // the source's own signatures of its landing and tag are taken
    let (_, mut h) = holder(&src, "ok");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(h.signatures_of(&src.landing).unwrap().len(), 1);
    assert_eq!(h.signatures_of(&src.tag).unwrap().len(), 1);
}

#[test]
fn ha11_changed_relay_answer_refuses_before_append() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let relay = key(kinds::ANSWER, src.relay);
    // the relay answer's place in the file
    let (n, offset) = {
        let (mut r, _) = StreamReader::open(File::open(&paths[0]).unwrap()).unwrap();
        loop {
            let rec = r.next().unwrap().expect("the relay answer is in the file");
            if rec.key() == relay {
                break r.position();
            }
        }
    };
    // a holder that holds every immutable record of the upload, the relay answer included, and
    // no tag or answer ref
    let (path, mut h) = holder(&src, "h");
    let items: Vec<(u8, Vec<u8>)> = {
        let records = src.closure(src.tag);
        let (_s, r) = src.reader();
        let mut source = StoreSource {
            reader: &r,
            end: END_OF_LOG,
        };
        records
            .into_iter()
            .map(|k| match source.record(k).unwrap().unwrap() {
                // the raw frame: its raw kind and payload
                r @ StreamRecord::Present { .. } => {
                    let raw = r.raw_kind();
                    let StreamRecord::Present { payload, .. } = r else {
                        unreachable!("a present record")
                    };
                    (raw, payload)
                }
                other => panic!("{other:?}"),
            })
            .collect()
    };
    h.write(items).unwrap();
    assert!(h.has_kind(kinds::ANSWER, &src.relay).unwrap());
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    let before = (segment_bytes(&path), h.log_end());
    // the caller's check changes a byte of the answer's payload, then chooses the incoming tag
    let p0 = paths[0].clone();
    let got = h.take_batches(&open(&paths), &src.target(), move |_, _| {
        let mut bytes = std::fs::read(&p0).unwrap();
        bytes[offset as usize + 37] ^= 0xFF;
        std::fs::write(&p0, &bytes).unwrap();
        Ok::<_, StreamError>(TagChoice::Incoming)
    });
    let e = stream_err(got.unwrap_err());
    assert_eq!(
        (e.code, e.message),
        (
            "E_BAD_RECORD",
            format!("stream record {n} at byte {offset}: input changed during take")
        )
    );
    assert_eq!(h.log_end(), before.1);
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    assert_eq!(h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before.0);
}

/// Fix round 2: the take installs `answer/tag-object:<root>` only for an answer that targets
/// exactly the root TAG. Two tag objects of one name at one landing (A held, B uploaded): an
/// answer to `tag:v1` that targets A, or no object, is not B's relay answer, so an upload of B
/// that carries it is refused before any append; one that targets B and A is an answer of B,
/// taken with no ref; and B's own relay answer gets B's ref.
#[test]
fn ha13_take_names_only_the_relay_answer_of_its_own_tag_object() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let (path, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    // B: a second tag object named v1 at the same landing, as a clone tags it
    let b = Tag {
        name: "v1".into(),
        landing: src.landing,
        message: Some("B".into()),
        tagged_by: prov("human:alice", Role::Approver),
    };
    let b_bytes = enc(&b);
    let b_hash = Hash::of(&b_bytes);
    assert_ne!(b_hash, src.tag);
    let existing = |_: &mut dyn ObjectSource, _: &UploadFacts| {
        Ok::<_, StreamError>(TagChoice::Existing {
            name: "v1".into(),
            hash: src.tag,
        })
    };
    // an upload of B and an answer to tag:v1 with `targets`: the answer's hash and the file
    let upload = |name: &str, targets: Vec<Hash>| {
        let a = enc(&answer("tag:v1", targets.clone(), Some(name)));
        let file = src.dir.path().join(format!("{name}.rdysync"));
        write_extra(
            &src.repo,
            b_hash,
            &[(kinds::TAG, b_bytes.clone()), (kinds::ANSWER, a.clone())],
            // the exact prerequisites: the landing, and A when the answer targets it
            std::iter::once(key(kinds::LANDING, src.landing))
                .chain(targets.contains(&src.tag).then(|| key(kinds::TAG, src.tag)))
                .collect(),
            &file,
        );
        (Hash::of(&a), file)
    };
    // crossed: an answer that targets A (the held tag of the same name), and one with no target
    for (name, targets) in [("crossed", vec![src.tag]), ("none", vec![])] {
        let (a, file) = upload(name, targets);
        let before = (segment_bytes(&path), h.log_end());
        let e = stream_err(
            h.take_batches(&open(&[file]), &src.target(), existing)
                .unwrap_err(),
        );
        assert_eq!(
            e.message, "stream record 0 at byte 0: object is outside the tag closure",
            "{name}"
        );
        assert_eq!(h.log_end(), before.1);
        assert_eq!(segment_bytes(&path), before.0);
        assert!(!h.has_kind(kinds::TAG, &b_hash).unwrap());
        assert!(!h.has_kind(kinds::ANSWER, &a).unwrap());
        assert_eq!(h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(), None);
        h.check_holder().unwrap();
    }
    // an answer that targets B and A: attached to B by its target, taken, but not B's relay
    // answer
    let (loose, file) = upload("loose", vec![b_hash, src.tag]);
    h.take_batches(&open(&[file]), &src.target(), existing)
        .unwrap();
    assert!(h.has_kind(kinds::TAG, &b_hash).unwrap());
    assert!(h.has_kind(kinds::ANSWER, &loose).unwrap());
    assert_eq!(h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(), None);
    h.check_holder().unwrap();
    // B with its own relay answer: taken, and named by B's tag object
    let (own, file) = upload("own", vec![b_hash]);
    let got = h
        .take_batches(&open(&[file]), &src.target(), existing)
        .unwrap();
    assert_eq!(got.tag_hash, src.tag);
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(),
        Some(own.0.to_vec())
    );
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    h.check_holder().unwrap();
}

/// The source copied to `removed` and then a person's removal of alpha applied there (the
/// shared raw fixture), its erasure finished: the copy's path, the request and the apply.
struct Removed {
    path: PathBuf,
    request: Hash,
    apply: Hash,
}

fn removed_copy(src: &Src) -> Removed {
    let path = src.dir.path().join("removed");
    carried_fixture::copy_store(&src.path, &path);
    let fixture = redaction_fixture::authorized_batch_fixture(
        &path,
        &[redaction_fixture::RequestSpec::new(
            "work",
            vec![rdy_model::redaction::RedactTarget::Version {
                id: src.alpha.id.clone(),
                version: src.alpha.version_hash(),
            }],
        )
        .at(60)],
        &redaction_fixture::FixtureOptions::approved(redaction_fixture::Proof::local_human()),
    );
    drop(Store::open(&path).unwrap());
    Removed {
        path,
        request: fixture.requests[0].request,
        apply: fixture.applies[0].0,
    }
}

/// The whole history of `tag` in the store at `path`, as one batch file `name`.
fn upload_of(src: &Src, path: &Path, tag: Hash, name: &str) -> PathBuf {
    let s = Store::open(path).unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let file = src.dir.path().join(format!("{name}.rdysync"));
    write(&r, &src.repo, tag, &c.records, c.prerequisites, &file);
    file
}

/// The change that wrote alpha (and beta) in the source.
fn alpha_change(src: &Src) -> Change {
    let s = Store::open(&src.path).unwrap();
    let id = s.producers(&src.alpha.version_hash()).unwrap()[0];
    s.change(&id).unwrap()
}

/// A holder that took v1 from the source before the removal, loaded into `name` with one
/// carried record of `request` and `apply` over its own frames of alpha (block and change) and,
/// with `beta`, beta's block, each `held`. Returns the holder's path and the record.
fn carried_holder(
    src: &Src,
    name: &str,
    request: Hash,
    apply: Hash,
    held: bool,
    beta: bool,
) -> (PathBuf, rdy_model::redaction::RedactionCarried) {
    use carried_fixture::{block_residue, change_residue, find, framed_at, span};
    let pre = src.dir.path().join(format!("{name}-pre"));
    {
        let mut h = Store::create_holder(&pre, src.repo).unwrap();
        take(&mut h, &open(&src.split(src.tag, 1, name)), &src.target()).unwrap();
    }
    let frames = framed_at(&pre);
    let c = alpha_change(src);
    let beta_block = block("b-00002", "beta");
    let mut residues = vec![block_residue(&src.alpha), change_residue(&c)];
    let mut spans = vec![
        span(
            &find(&frames, kinds::BLOCK, src.alpha.version_hash()),
            Some(&residues[0]),
            held,
        ),
        span(
            &find(&frames, kinds::CHANGE, c.id()),
            Some(&residues[1]),
            held,
        ),
    ];
    if beta {
        residues.push(block_residue(&beta_block));
        spans.push(span(
            &find(&frames, kinds::BLOCK, beta_block.version_hash()),
            Some(&residues[2]),
            held,
        ));
    }
    spans.sort_by_key(|s| s.place);
    let mut rec = carried_fixture::record(
        vec![rdy_model::redaction::RedactTarget::Version {
            id: src.alpha.id.clone(),
            version: src.alpha.version_hash(),
        }],
        spans,
    );
    rec.request = request;
    rec.apply = apply;
    let to = src.dir.path().join(name);
    carried_fixture::load(&pre, &to, &residues, std::slice::from_ref(&rec)).unwrap();
    (to, rec)
}

/// The proof the record walker names for the dropped frame of `kind` and `hash`.
fn proof_of(h: &Store, kind: u8, hash: Hash) -> Option<Hash> {
    let mut out = None;
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |_, f, _| {
            if let StoredFrame::Dropped {
                kind: k,
                hash: x,
                apply,
                ..
            } = f
                && (rdy_store::codec::logical_kind(k), x) == (kind, hash)
            {
                out = Some(apply);
            }
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn hc3_take_of_any_matching_apply_removes_held_payloads() {
    let src = source();
    let removed = removed_copy(&src);
    let upload = upload_of(&src, &removed.path, src.tag, "removed");
    let alpha = src.alpha.version_hash();
    let c = alpha_change(&src).id();
    // the source's own apply removes the held payloads its carried record names
    let (path, rec) = carried_holder(&src, "own", removed.request, removed.apply, true, false);
    let mut h = Store::open(&path).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (2, 0));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (0, 2));
    assert_eq!(
        h.get_block(&alpha).unwrap().body,
        rdy_model::redaction::CONTENT_REMOVED
    );
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
    drop(h);
    // another request's apply counts too; a partial take erases only the spans it covers
    let (path, rec) = carried_holder(
        &src,
        "other",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        true,
        true,
    );
    let mut h = Store::open(&path).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (3, 0));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (1, 2));
    let beta = block("b-00002", "beta");
    assert_eq!(h.get_block(&beta.version_hash()).unwrap(), beta);
    assert_eq!(proof_of(&h, kinds::CHANGE, c), Some(removed.apply));
    drop(h);
    // an apply over zeros a carried record explained becomes their proof
    let (path, rec) = carried_holder(
        &src,
        "zeros",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        false,
        false,
    );
    let carried = carried_fixture::carried_hash(&rec);
    let mut h = Store::open(&path).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(carried));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
    assert_eq!(proof_of(&h, kinds::CHANGE, c), Some(removed.apply));
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (0, 2));
    let path = h.dir().to_path_buf();
    drop(h);
    // the reopened holder derives the same proofs
    let h = Store::open(&path).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
}

/// The upload of `removed` with every placeholder's proof replaced by `proof`, which the header
/// lists as a prerequisite.
fn forged_upload(src: &Src, removed: &Removed, proof: Hash, name: &str) -> (PathBuf, ObjectKey) {
    let honest = upload_of(src, &removed.path, src.tag, &format!("{name}-honest"));
    let bytes = std::fs::read(&honest).unwrap();
    let (mut r, header) = StreamReader::open(&bytes[..]).unwrap();
    let mut keys = vec![];
    let mut records: BTreeMap<ObjectKey, StreamRecord> = BTreeMap::new();
    let mut placeholder = None;
    while let Some(rec) = r.next().unwrap() {
        let rec = match rec {
            StreamRecord::Dropped {
                key,
                original_len,
                residue,
                ..
            } => {
                placeholder.get_or_insert(key);
                StreamRecord::Dropped {
                    encoding: 0,
                    key,
                    original_len,
                    apply: proof,
                    residue,
                }
            }
            other => other,
        };
        keys.push(rec.key());
        records.insert(rec.key(), rec);
    }
    r.finish().unwrap();
    let mut prerequisites = header.prerequisites.clone();
    prerequisites.push(key(kinds::REDACTION_APPLY, proof));
    prerequisites.sort();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        src.repo,
        src.tag,
        prerequisites,
    );
    let file = src.dir.path().join(format!("{name}.rdysync"));
    write_batch(&mut records, &header, &keys, File::create(&file).unwrap()).unwrap();
    (file, placeholder.expect("a placeholder"))
}

#[test]
fn hc3_take_refuses_a_placeholder_naming_a_carried_record() {
    let src = source();
    let removed = removed_copy(&src);
    let (path, rec) = carried_holder(&src, "h", removed.request, removed.apply, true, false);
    let carried = carried_fixture::carried_hash(&rec);
    let (forged, placeholder) = forged_upload(&src, &removed, carried, "forged");
    let mut h = Store::open(&path).unwrap();
    let before = (segment_bytes(&path), h.log_end());
    let e = stream_err(take(&mut h, &open(&[forged]), &src.target()).unwrap_err());
    assert_eq!(e.code, "E_BAD_RECORD");
    assert!(
        e.message.ends_with("invalid removal proof"),
        "{}",
        e.message
    );
    assert_eq!(e.object, Some(placeholder));
    // nothing was written, and the held payloads are still held
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (2, 0));
    let end = h.log_end();
    drop(h);
    assert_eq!((segment_bytes(&path), end), before);
}

#[test]
fn hc3_a_placeholder_naming_an_invalid_carried_record_is_refused() {
    let src = source();
    let removed = removed_copy(&src);
    // the holder's carried record explains zeros; it is still no removal proof for a take
    let (path, rec) = carried_holder(
        &src,
        "h",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        false,
        false,
    );
    let carried = carried_fixture::carried_hash(&rec);
    let (forged, placeholder) = forged_upload(&src, &removed, carried, "forged");
    let mut h = Store::open(&path).unwrap();
    let before = segment_bytes(&path);
    let e = stream_err(take(&mut h, &open(&[forged]), &src.target()).unwrap_err());
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_RECORD",
            "stream record 0 at byte 0: invalid removal proof",
            "request this stream again; run rdy fsck at its source"
        )
    );
    assert_eq!(e.object, Some(placeholder));
    drop(h);
    assert_eq!(segment_bytes(&path), before);
}

/// A project whose one landing is tagged `v1` by a TAG written through `Store::put` (so in the
/// TAG kind's latest encoding), and one batch file of the tag's whole closure whose header
/// carries `features`. Returns the base directory, the project path, its repo, the landing, the
/// tag and the batch file.
fn put_tag_upload(
    features: rdy_store::format::StoreFeatures,
) -> (tempfile::TempDir, PathBuf, RepoId, Hash, Hash, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
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
            prov("agent:w", Role::Writer),
            vec![cid],
            None,
        )
        .unwrap();
    let landing = s
        .land(&proposal, prov("human:alice", Role::Approver))
        .unwrap();
    let tag = s
        .put(
            kinds::TAG,
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
        )
        .unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let file = dir.path().join("v1.rdysync");
    let header = StreamHeader::tag_history(features, repo, tag, c.prerequisites);
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(&file).unwrap(),
    )
    .unwrap();
    drop((r, s));
    (dir, path, repo, landing, tag, file)
}

fn manifest_of(s: &Store) -> rdy_store::format::StoreFeatures {
    rdy_store::format::StoreFeatures::parse(
        &s.get_ref(rdy_store::format::REF_STORE_FEATURES)
            .unwrap()
            .expect("the features ref"),
    )
    .unwrap()
}

#[test]
fn ok1_a_versioned_frame_is_taken_with_its_raw_kind() {
    use rdy_store::codec;
    let _v1 = codec::test_encoding(kinds::TAG, 1);
    let (dir, path, repo, landing, tag, file) =
        put_tag_upload(rdy_store::format::baseline_features());
    let (src_kind, src_payload) = Store::open(&path).unwrap().get_raw(&tag).unwrap();
    assert_eq!(
        src_kind,
        kinds::TAG | codec::VERSIONED,
        "the source's raw kind"
    );
    assert_eq!(&src_payload[..2], &[1, 0]);
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    take(&mut h, &open(&[file]), &target).unwrap();
    // the holder keeps the frame's raw kind and exact bytes, and reads it as a TAG
    let check = |h: &Store| {
        assert_eq!(
            h.get_raw(&tag).unwrap(),
            (src_kind, src_payload.clone()),
            "the raw frame as the source stored it"
        );
        let t: Tag = h.get_typed(kinds::TAG, &tag).unwrap();
        assert_eq!((t.name.as_str(), t.landing), ("v1", landing));
        assert_eq!(h.get_ref("tag/v1").unwrap(), Some(tag.0.to_vec()));
        h.check_meta().unwrap();
        h.check_holder().unwrap();
    };
    check(&h);
    h.reindex().unwrap();
    check(&h);
    drop(h);
    let h = Store::open(&hpath).unwrap();
    check(&h);
    assert!(h.verify().unwrap() > 0);
}

#[test]
fn ok1_take_refuses_a_stream_this_holder_may_only_read() {
    use rdy_store::format::{FeatureClass, StoreFeature, StoreFeatures};
    let read_only = StoreFeatures::of(vec![StoreFeature {
        name: "future_read_only".into(),
        class: FeatureClass::ReadOnlyCompatible,
        required_rdy: "99.0.0".into(),
    }]);
    let (dir, _path, repo, landing, _tag, file) = put_tag_upload(read_only);
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let end = h.log_end();
    let before = segment_bytes(&hpath);
    let format_before = std::fs::read(hpath.join(rdy_store::format::FEATURES_FILE)).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let e = stream_err(take(&mut h, &open(&[file]), &target).unwrap_err());
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_STORE_FORMAT",
            "store feature future_read_only needs rdy 99.0.0",
            "open the project with rdy 99.0.0 or later"
        )
    );
    assert_eq!(h.log_end(), end, "nothing appended");
    assert_eq!(manifest_of(&h), rdy_store::format::baseline_features());
    drop(h);
    assert_eq!(segment_bytes(&hpath), before);
    assert_eq!(
        std::fs::read(hpath.join(rdy_store::format::FEATURES_FILE)).unwrap(),
        format_before
    );
}

#[test]
fn ok1_take_keeps_the_stream_features_and_activates_first_use() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, StoreFeature, StoreFeatures, TEST_FEATURE, inject_test_feature,
    };
    // an unknown compatible feature of the source, and the test feature that a TAG uses
    let future = StoreFeature {
        name: "zz_future".into(),
        class: FeatureClass::Compatible,
        required_rdy: "99.1.0".into(),
    };
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    let (dir, _path, repo, landing, tag, file) =
        put_tag_upload(StoreFeatures::of(vec![future.clone()]));
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let end = h.log_end();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let got = take(&mut h, &open(&[file]), &target).unwrap();
    let want = rdy_store::format::baseline_features().merged(&[
        StoreFeature {
            name: TEST_FEATURE.into(),
            class: FeatureClass::Compatible,
            required_rdy: "99.0.0".into(),
        },
        future,
    ]);
    assert_eq!(
        manifest_of(&h),
        want,
        "the stream's and the first use's features"
    );
    assert_eq!(h.features(), &want);
    assert_eq!(
        std::fs::read_to_string(hpath.join(FEATURES_FILE)).unwrap(),
        want.to_text(),
        "FORMAT mirrors the log"
    );
    // one sealed batch: the closure, the tag ref and the features ref
    assert_eq!(markers_since(&h, end), 1);
    assert_eq!(got.frames, got.new_objects + 2);
    assert!(h.has_kind(kinds::TAG, &tag).unwrap());
    h.check_meta().unwrap();
    // a repeat activates nothing more
    drop(h);
    let h = Store::open(&hpath).unwrap();
    assert_eq!(manifest_of(&h), want);
}

/// Fix round 3: a take whose batch is synced and whose index update then fails is rolled back
/// only if the truncate works. When the truncate fails too, the marked batch stays in the log,
/// so `FORMAT` keeps the manifest the take published and the holder reopens with it.
#[test]
fn ok1_a_take_that_cannot_roll_back_keeps_format() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, StoreFeature, StoreFeatures, TEST_FEATURE, inject_test_feature,
    };
    let future = StoreFeature {
        name: "zz_future".into(),
        class: FeatureClass::Compatible,
        required_rdy: "99.1.0".into(),
    };
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    let (dir, _path, repo, landing, _tag, file) =
        put_tag_upload(StoreFeatures::of(vec![future.clone()]));
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let want = rdy_store::format::baseline_features().merged(&[
        StoreFeature {
            name: TEST_FEATURE.into(),
            class: FeatureClass::Compatible,
            required_rdy: "99.0.0".into(),
        },
        future,
    ]);
    h.fail_next_index_apply();
    h.fail_next_rollback();
    assert!(take(&mut h, &open(&[file]), &target).is_err());
    assert_eq!(
        std::fs::read_to_string(hpath.join(FEATURES_FILE)).unwrap(),
        want.to_text(),
        "FORMAT keeps the published manifest"
    );
    drop(h);
    let h = Store::open(&hpath).expect("the holder reopens");
    assert_eq!(manifest_of(&h), want, "the marked batch is the log's");
    assert_eq!(h.features(), &want);
}

/// `n` unknown compatible features named `{prefix}000` on.
fn unknown_features(prefix: &str, n: usize) -> Vec<rdy_store::format::StoreFeature> {
    (0..n)
        .map(|i| rdy_store::format::StoreFeature {
            name: format!("{prefix}{i:03}"),
            class: rdy_store::format::FeatureClass::Compatible,
            required_rdy: "1.0.0".into(),
        })
        .collect()
}

/// D-WWAK7: the manifest a take would leave (the holder's, the stream header's and the take's
/// first uses) keeps the manifest's bounds. A union of more than 256 features is refused before
/// `FORMAT` or the log changes, whichever parts make it up; one that reaches the bound lands.
#[test]
fn ok1_take_refuses_a_merged_manifest_over_its_bounds() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, REF_STORE_FEATURES, inject_test_feature, write_format_file,
    };
    // the TAG of every upload first uses the test feature
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    // (holder's unknown features, header's unknown features, refused): each manifest also
    // names the two baseline features (ok4), and the take first uses the test feature, so the
    // merged manifest holds 3 more than the two counts
    let cases: Vec<(usize, usize, bool)> = vec![
        (0, 254, true),   // a full header and the first use
        (200, 54, true),  // the holder and the header
        (253, 1, true),   // the holder, the header and the first use
        (128, 126, true), // half and half, and the first use
        (252, 1, false),  // exactly the bound
        (0, 253, false),  // exactly the bound
    ];
    for (held, carried, refused) in cases {
        let case = format!("holder {held}, header {carried}");
        let header = rdy_store::format::baseline_features().merged(&unknown_features("h", carried));
        let (dir, _path, repo, landing, tag, file) = put_tag_upload(header);
        let hpath = dir.path().join("h");
        let mut h = Store::create_holder(&hpath, repo).unwrap();
        let mine = rdy_store::format::baseline_features().merged(&unknown_features("a", held));
        if held > 0 {
            h.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), mine.to_json())])
                .unwrap();
            write_format_file(&hpath, &mine).unwrap();
            drop(h);
            h = Store::open(&hpath).unwrap();
        }
        assert_eq!(h.features(), &mine, "{case}");
        let end = h.log_end();
        let before = segment_bytes(&hpath);
        let format_before = std::fs::read(hpath.join(FEATURES_FILE)).unwrap();
        let target = UploadTarget {
            tag: "v1".into(),
            landing,
        };
        let got = take(&mut h, &open(&[file]), &target);
        if !refused {
            got.unwrap();
            assert_eq!(manifest_of(&h).features.len(), 256, "{case}");
            assert_eq!(h.features(), &manifest_of(&h), "{case}");
            assert!(h.has_kind(kinds::TAG, &tag).unwrap(), "{case}");
            drop(h);
            Store::open(&hpath).unwrap().check_meta().unwrap();
            continue;
        }
        let e = stream_err(got.unwrap_err());
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_STORE_FORMAT",
                "store feature manifest is invalid: more than 256 features",
                "restore the store from a backup"
            ),
            "{case}"
        );
        assert_eq!(h.log_end(), end, "{case}: nothing appended");
        assert_eq!(h.features(), &mine, "{case}");
        assert!(!h.has_kind(kinds::TAG, &tag).unwrap(), "{case}");
        drop(h);
        assert_eq!(segment_bytes(&hpath), before, "{case}");
        assert_eq!(
            std::fs::read(hpath.join(FEATURES_FILE)).unwrap(),
            format_before,
            "{case}"
        );
        let h = Store::open(&hpath).unwrap();
        assert_eq!(manifest_of(&h), mine, "{case}");
        h.check_meta().unwrap();
    }
}

/// The batch markers of `h`'s log from `from` on.
fn markers_since(h: &Store, from: rdy_store::segment::Location) -> usize {
    log_frames(h)
        .iter()
        .filter(|(l, k, _, _)| *l >= from && *k == kinds::BATCH_COMMIT)
        .count()
}

// ---------------------------------------------------------------------------------------------
// tcw (TASK-2324; D-BPYN8, p-VCW8D): the take's closure walk ends once every upload key is
// reached, and goes on through the holder while one is not

/// Lands `n` more changes on `s`, each creating one new block, and returns the last landing.
fn tcw_land(s: &mut Store, from: usize, n: usize) -> Hash {
    let mut last = None;
    for i in from..from + n {
        s.commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block(&format!("b-{:05}", 10 + i), &format!("body {i}")),
                }],
            ),
        )
        .unwrap();
        let p = s
            .propose("work", &format!("p{i}"), prov("agent:w", Role::Writer))
            .unwrap();
        last = Some(s.land(&p, prov("human:alice", Role::Approver)).unwrap());
    }
    last.expect("at least one landing")
}

/// Tags `landing` as `name` with its relay answer; returns the tag.
fn tcw_tag(s: &mut Store, name: &str, landing: Hash) -> Hash {
    s.put_tag_answered(
        &Tag {
            name: name.into(),
            landing,
            message: None,
            tagged_by: prov("human:alice", Role::Approver),
        },
        answer(&format!("tag:{name}"), vec![], None),
    )
    .unwrap()
    .0
}

/// A source with `n` landings tagged `v1` (as [`source`]'s fields give it: `landing` is the
/// first landing, `tag` is `v1`), and a holder that holds `v1`'s whole closure.
fn tcw_held_history(n: usize) -> (Src, Store) {
    let src = source();
    let mut s = Store::open(&src.path).unwrap();
    let last = tcw_land(&mut s, 0, n);
    let v1 = tcw_tag(&mut s, "v1.1", last);
    drop(s);
    let (_, mut h) = holder(&src, "h");
    let paths = src.split(v1, 1, "v1-full");
    let target = UploadTarget {
        tag: "v1.1".into(),
        landing: last,
    };
    take(&mut h, &open(&paths), &target).unwrap();
    (Src { tag: v1, ..src }, h)
}

/// Lands `n` more changes past `held`'s tag and tags them `v2`; returns the target and the batch
/// of `v2`'s closure less what `known` holds.
fn tcw_next(
    src: &Src,
    n: usize,
    known: &BTreeSet<ObjectKey>,
    name: &str,
) -> (UploadTarget, PathBuf, Vec<ObjectKey>, Hash) {
    let mut s = Store::open(&src.path).unwrap();
    let last = tcw_land(&mut s, 50_000, n);
    let v2 = tcw_tag(&mut s, "v2", last);
    drop(s);
    let (_s, r) = src.reader();
    let c = closure(&r, v2, known).unwrap();
    let path = src.dir.path().join(format!("{name}.rdysync"));
    write(&r, &src.repo, v2, &c.records, c.prerequisites, &path);
    (
        UploadTarget {
            tag: "v2".into(),
            landing: last,
        },
        path,
        c.records,
        v2,
    )
}

fn tcw_known(src: &Src) -> BTreeSet<ObjectKey> {
    src.closure(src.tag).into_iter().collect()
}

#[test]
fn tcw_new_records_past_a_long_held_history_are_taken() {
    let (src, mut h) = tcw_held_history(60);
    let known = tcw_known(&src);
    let (target, path, records, _) = tcw_next(&src, 2, &known, "new");
    assert!(records.iter().all(|k| !known.contains(k)));
    let got = take(&mut h, &open(&[path]), &target).unwrap();
    assert_eq!(got.new_objects as usize, records.len());
    assert!(h.get_ref("tag/v2").unwrap().is_some());
    h.check_holder().unwrap();
}

#[test]
fn tcw_a_record_outside_the_root_closure_is_still_refused() {
    let (src, mut h) = tcw_held_history(60);
    let known = tcw_known(&src);
    let (target, path, _, v2) = tcw_next(&src, 2, &known, "new");
    let stray = src.dir.path().join("stray.rdysync");
    write_extra(
        &src.repo,
        v2,
        &[(kinds::BLOCK, enc(&block("b-99999", "stray")))],
        vec![key(kinds::TAG, v2)],
        &stray,
    );
    let end = h.log_end();
    let e = stream_err(take(&mut h, &open(&[path, stray]), &target).unwrap_err());
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(
        e.object,
        Some(key(
            kinds::BLOCK,
            Hash::of(&enc(&block("b-99999", "stray")))
        ))
    );
    assert_eq!(h.log_end(), end);
    assert_eq!(h.get_ref("tag/v2").unwrap(), None);
}

#[test]
fn tcw_a_new_record_attached_to_an_old_held_landing_is_taken() {
    let (src, mut h) = tcw_held_history(60);
    let known = tcw_known(&src);
    // a later signature of the first landing, which the holder holds since v1
    assert!(h.has_kind(kinds::LANDING, &src.landing).unwrap());
    let mut s = Store::open(&src.path).unwrap();
    let late = s
        .put_signature(&sig(src.landing, "rdy-approval-v1", 9))
        .unwrap();
    drop(s);
    let (target, path, records, _) = tcw_next(&src, 2, &known, "late");
    assert!(records.contains(&key(kinds::SIGNATURE, late)));
    take(&mut h, &open(&[path]), &target).unwrap();
    assert!(h.has_kind(kinds::SIGNATURE, &late).unwrap());
    h.check_holder().unwrap();
}

#[test]
fn tcw_a_full_layout_that_resends_held_records_is_taken() {
    let (src, mut h) = tcw_held_history(60);
    let known = tcw_known(&src);
    let (target, path, records, _) = tcw_next(&src, 2, &BTreeSet::new(), "full");
    // every held landing, snapshot and change is sent again (only v1.1's tag and its relay
    // answer are not in v2's closure)
    let resent = records.iter().filter(|k| known.contains(k)).count();
    assert!(resent + 2 >= known.len(), "{resent} of {}", known.len());
    let got = take(&mut h, &open(&[path]), &target).unwrap();
    assert_eq!(got.new_objects as usize, records.len() - resent);
    h.check_holder().unwrap();
}

// tcw (TASK-2340; D-BPYN8, p-7QDPC): the take keeps its holder within what the holder check
// reads, whether the closure walk ended early or not

/// Sets the take's capacity bounds on this thread for the guard's life.
struct Capacity;

impl Capacity {
    fn set(landing_depth: u64, objects: u64, edges: u64) -> Capacity {
        set_holder_capacity_for_tests(Some(HolderCapacity {
            landing_depth,
            objects,
            edges,
        }));
        Capacity
    }
}

impl Drop for Capacity {
    fn drop(&mut self) {
        set_holder_capacity_for_tests(None);
    }
}

/// What a holder holds after taking `v2` too: its counts and `v2`'s landing depth, read from a
/// second holder that takes the same files with the default bounds.
fn tcw_after(
    src: &Src,
    v1_target: &UploadTarget,
    v1: &[PathBuf],
    target: &UploadTarget,
    v2: &Path,
) -> (u64, u64, u64) {
    let (_, mut other) = holder(src, "other");
    take(&mut other, &open(v1), v1_target).unwrap();
    take(&mut other, &open(&[v2.to_path_buf()]), target).unwrap();
    let (objects, edges) = other.reader().holder_counts().unwrap();
    let depth = other
        .reader()
        .landing_depth(&target.landing)
        .unwrap()
        .unwrap();
    (objects, edges, depth)
}

fn capacity_err(e: TakeError<StreamError>, text: &str) {
    let e = stream_err(e);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            text,
            "keep the project; this holder takes no more history until its limit is raised"
        )
    );
}

/// A source of `n` landings tagged `v1.1`, its whole-closure files and target, a holder that
/// holds them, and the new-only upload of `v2` (two landings more).
fn tcw_capacity_setup(
    n: usize,
) -> (
    Src,
    UploadTarget,
    Vec<PathBuf>,
    Store,
    UploadTarget,
    PathBuf,
) {
    let src = source();
    let mut s = Store::open(&src.path).unwrap();
    let last = tcw_land(&mut s, 0, n);
    let v1 = tcw_tag(&mut s, "v1.1", last);
    drop(s);
    let v1_target = UploadTarget {
        tag: "v1.1".into(),
        landing: last,
    };
    let v1_paths = src.split(v1, 1, "v1-full");
    let (_, mut h) = holder(&src, "h");
    take(&mut h, &open(&v1_paths), &v1_target).unwrap();
    let src = Src { tag: v1, ..src };
    let known = tcw_known(&src);
    let (target, path, _, _) = tcw_next(&src, 2, &known, "new");
    (src, v1_target, v1_paths, h, target, path)
}

#[test]
fn tcw_capacity_guard_refuses_one_past_each_bound_and_accepts_at_it() {
    let (src, v1_target, v1_paths, mut h, target, path) = tcw_capacity_setup(10);
    let (objects, edges, depth) = tcw_after(&src, &v1_target, &v1_paths, &target, &path);
    // the source's first landing, ten more, then v2's two
    assert_eq!(depth, 13);
    let end = h.log_end();
    for (bounds, text) in [
        (
            (depth, objects - 1, edges),
            format!("holder check exceeds {} objects", objects - 1),
        ),
        (
            (depth, objects, edges - 1),
            format!("holder check exceeds {} edges", edges - 1),
        ),
        (
            (depth - 1, objects, edges),
            format!("holder check exceeds {} landing depth", depth - 1),
        ),
    ] {
        let _c = Capacity::set(bounds.0, bounds.1, bounds.2);
        capacity_err(
            take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap_err(),
            &text,
        );
        assert_eq!(h.log_end(), end, "{text}");
        assert_eq!(h.get_ref("tag/v2").unwrap(), None, "{text}");
    }
    // at every bound the take is accepted, and the holder passes its check at the same bounds
    {
        let _c = Capacity::set(depth, objects, edges);
        take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap();
    }
    assert_eq!(h.reader().holder_counts().unwrap(), (objects, edges));
    h.check_holder_bounded_for_tests(depth as usize, objects as usize, edges as usize)
        .unwrap();
    // the depth is exact: one less is past the chain the check reads. The object and edge
    // counts are upper bounds (a held signature is no forward link of a tag, so the check may
    // read fewer)
    let err = h
        .check_holder_bounded_for_tests(depth as usize - 1, objects as usize, edges as usize)
        .unwrap_err();
    let text = format!("{} landing depth", depth - 1);
    assert_eq!(
        rdy_store::check_holder::holder_check_limit(&err),
        Some(text.as_str())
    );
}

#[test]
fn tcw_early_exit_take_is_refused_once_the_holder_would_pass_its_bounds() {
    // the case the closure walk's early exit accepts with no holder bound: new records only,
    // past a long held history
    let (src, v1_target, v1_paths, mut h, target, path) = tcw_capacity_setup(60);
    let (objects, edges, depth) = tcw_after(&src, &v1_target, &v1_paths, &target, &path);
    let (held, _) = h.reader().holder_counts().unwrap();
    assert!(held < objects);
    // a holder full already refuses it
    {
        let _c = Capacity::set(depth, held, edges);
        capacity_err(
            take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap_err(),
            &format!("holder check exceeds {held} objects"),
        );
    }
    // one that fits takes it, and passes its check at the same bounds
    let _c = Capacity::set(depth, objects, edges);
    take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap();
    h.check_holder_bounded_for_tests(depth as usize, objects as usize, edges as usize)
        .unwrap();
}

#[test]
fn tcw_capacity_counts_and_depths_survive_reopen_and_reindex() {
    let (src, _, _, mut h, target, path) = tcw_capacity_setup(30);
    take(&mut h, &open(&[path]), &target).unwrap();
    let r = h.reader();
    let counts = r.holder_counts().unwrap();
    // the chain from v2's landing: the source's first landing, thirty more, then two
    assert_eq!(r.landing_depth(&target.landing).unwrap(), Some(33));
    assert_eq!(r.landing_depth(&src.landing).unwrap(), Some(1));
    let dump = h.index_dump_for_tests().unwrap();
    assert!(!dump.holder_counts.is_empty() && dump.landing_depths.len() == 33);
    drop(r);
    let root = src.dir.path().join("h");
    drop(h);
    let mut h = Store::open(&root).unwrap();
    assert_eq!(h.reader().holder_counts().unwrap(), counts);
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    h.reindex().unwrap();
    assert_eq!(h.reader().holder_counts().unwrap(), counts);
    assert_eq!(h.reader().landing_depth(&target.landing).unwrap(), Some(33));
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    h.check_holder().unwrap();
}

#[test]
fn tcw_an_index_without_the_capacity_tables_is_rebuilt_at_open() {
    // an index written before the capacity counts never reads them as zero: the open refuses
    // it and rebuilds it from the log, with the same counts and depths
    for table in ["holder_counts", "landing_depths", "landing_waits"] {
        let (src, _, _, mut h, target, path) = tcw_capacity_setup(5);
        take(&mut h, &open(&[path]), &target).unwrap();
        let dump = h.index_dump_for_tests().unwrap();
        let counts = h.reader().holder_counts().unwrap();
        drop(h);
        let root = src.dir.path().join("h");
        let file = root.join("index.redb");
        {
            let raw = redb::Database::create(&file).unwrap();
            let w = raw.begin_write().unwrap();
            let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(table);
            assert!(w.delete_table(def).unwrap(), "the table was there");
            w.commit().unwrap();
        }
        match rdy_store::index::Index::open(&file) {
            Err(rdy_store::StoreError::Db(m)) => assert_eq!(
                m,
                "the index has no capacity counts; it is rebuilt from the log"
            ),
            Err(e) => panic!("another refusal: {e}"),
            Ok(_) => panic!("an index without {table} opened"),
        }
        let h = Store::open(&root).unwrap();
        assert!(h.open_report().index_rebuilt, "{table}");
        assert_eq!(h.index_dump_for_tests().unwrap(), dump, "{table}");
        assert_eq!(h.reader().holder_counts().unwrap(), counts, "{table}");
    }
}

// tcw (TASK-2340 review F1, F2): the capacity counts and the landing depths are the same however
// the frames arrive: in any order, in one commit or many, across checkpoints and replay chunks,
// live, after a reindex and after an open that rebuilds the index

/// Deletes `table` from the index of the closed store at `root`, then opens the store: the open
/// rebuilds the index from the log.
fn tcw_rebuilt(root: &Path, table: &str) -> Store {
    {
        let raw = redb::Database::create(root.join("index.redb")).unwrap();
        let w = raw.begin_write().unwrap();
        let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(table);
        assert!(w.delete_table(def).unwrap(), "{table} was there");
        w.commit().unwrap();
    }
    let h = Store::open(root).unwrap();
    assert!(h.open_report().index_rebuilt, "{table}");
    h
}

/// The live index of `h` (the store at `root`) equals the index a reindex builds, and each index
/// an open rebuilds from the log when one capacity table is missing. Returns `h` reopened.
fn tcw_same_after_rebuilds(mut h: Store, root: &Path) -> Store {
    let dump = h.index_dump_for_tests().unwrap();
    h.reindex().unwrap();
    assert_eq!(h.index_dump_for_tests().unwrap(), dump, "reindex");
    drop(h);
    for table in ["holder_counts", "landing_depths", "landing_waits"] {
        let h = tcw_rebuilt(root, table);
        assert_eq!(
            h.index_dump_for_tests().unwrap(),
            dump,
            "rebuilt without {table}"
        );
    }
    Store::open(root).unwrap()
}

/// A landing hash of the index-level histories.
fn tcw_l(n: u8) -> Hash {
    Hash::of(&[b'l', n])
}

/// The orders the index-level tests apply a history in: log order, newest first, and twenty
/// fixed shuffles.
fn tcw_orders<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let mut out = vec![items.to_vec(), items.iter().rev().cloned().collect()];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..20 {
        let mut v = items.to_vec();
        for i in (1..v.len()).rev() {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            v.swap(i, (seed >> 33) as usize % (i + 1));
        }
        out.push(v);
    }
    out
}

/// The commit sizes the index-level tests cut an order into: one entry per commit up to all in
/// one commit.
fn tcw_cuts(n: usize) -> Vec<usize> {
    let mut cuts: Vec<usize> = vec![1, 2, 3, n.div_ceil(2), n];
    cuts.dedup();
    cuts
}

#[test]
fn tcw_landing_depths_are_exact_in_any_order_and_commit_bounds() {
    // the chain 1 <- 2 <- 3 <- 4 <- 5; a fork 2 <- 6 <- 7 (3 and 6 share prev 2); and 9 <- 10,
    // whose 9 names a prev (8) never indexed. Landings 3 and 7 come twice, as a repeated frame
    let p = |n: u8| Some(tcw_l(n));
    let history = vec![
        (tcw_l(1), None),
        (tcw_l(2), p(1)),
        (tcw_l(3), p(2)),
        (tcw_l(4), p(3)),
        (tcw_l(5), p(4)),
        (tcw_l(6), p(2)),
        (tcw_l(7), p(6)),
        (tcw_l(9), p(8)),
        (tcw_l(10), p(9)),
        (tcw_l(3), p(2)),
        (tcw_l(7), p(6)),
    ];
    // the holder check's chain lengths
    let depths: BTreeMap<Vec<u8>, Vec<u8>> =
        [(1, 1u64), (2, 2), (3, 3), (4, 4), (5, 5), (6, 3), (7, 4)]
            .into_iter()
            .map(|(n, d)| (tcw_l(n).0.to_vec(), d.to_le_bytes().to_vec()))
            .collect();
    let waits: BTreeMap<Vec<u8>, Vec<u8>> = [(8, 9), (9, 10)]
        .into_iter()
        .map(|(prev, l)| ([tcw_l(prev).0, tcw_l(l).0].concat(), vec![]))
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let mut n = 0;
    for order in tcw_orders(&history) {
        for cut in tcw_cuts(order.len()) {
            n += 1;
            let ix = rdy_store::index::Index::open(&dir.path().join(format!("{n}.redb"))).unwrap();
            for (i, part) in order.chunks(cut).enumerate() {
                let entries: Vec<IndexEntry> = part
                    .iter()
                    .map(|(landing, prev)| IndexEntry::LandingDepth {
                        landing: *landing,
                        prev: *prev,
                    })
                    .collect();
                let upto = rdy_store::segment::Location {
                    segment: 0,
                    offset: i as u64,
                };
                ix.apply(&entries, upto).unwrap();
            }
            let dump = ix.dump().unwrap();
            assert_eq!(dump.landing_depths, depths, "order {n}, {cut} per commit");
            assert_eq!(dump.landing_waits, waits, "order {n}, {cut} per commit");
            assert_eq!(ix.landing_depth(&tcw_l(10)).unwrap(), None);
        }
    }
}

#[test]
fn tcw_object_links_are_charged_once_in_any_commit_bounds() {
    // a TAG of one link four times, a CHANGE of three links twice, a BLOCK of none: three
    // objects and four links, however the frames split into commits
    let (t, c, b) = (tcw_l(1), tcw_l(2), tcw_l(3));
    let frames = [
        (kinds::TAG, t, 1u64),
        (kinds::CHANGE, c, 3),
        (kinds::TAG, t, 1),
        (kinds::BLOCK, b, 0),
        (kinds::TAG, t, 1),
        (kinds::CHANGE, c, 3),
        (kinds::TAG, t, 1),
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut n = 0;
    for cut in 1..=frames.len() {
        n += 1;
        let ix = rdy_store::index::Index::open(&dir.path().join(format!("{n}.redb"))).unwrap();
        for (i, part) in frames.chunks(cut).enumerate() {
            let mut entries = vec![];
            for (j, (kind, hash, links)) in part.iter().enumerate() {
                entries.push(IndexEntry::Object {
                    kind: *kind,
                    hash: *hash,
                    loc: rdy_store::segment::Location {
                        segment: 0,
                        offset: (i * cut + j) as u64,
                    },
                });
                if *links > 0 {
                    entries.push(IndexEntry::Links {
                        kind: *kind,
                        hash: *hash,
                        links: *links,
                    });
                }
            }
            let upto = rdy_store::segment::Location {
                segment: 0,
                offset: (i * cut + part.len()) as u64,
            };
            ix.apply(&entries, upto).unwrap();
        }
        assert_eq!(ix.holder_counts().unwrap(), (3, 4), "{cut} per commit");
    }
}

#[test]
fn tcw_landings_indexed_newest_first_keep_exact_depths_and_take_decisions() {
    // a holder whose landing depths were folded newest first, all in one commit or one landing
    // per commit (each prev arrives in a later commit than the landing that names it), holds
    // the depths a log-order fold gives, and a take at the depth bound is decided as the holder
    // check decides
    for per_commit in [false, true] {
        let (src, v1_target, v1_paths, h, target, path) = tcw_capacity_setup(5);
        let (objects, edges, depth) = tcw_after(&src, &v1_target, &v1_paths, &target, &path);
        // the source's first landing, five more, then v2's two
        assert_eq!(depth, 8);
        let chain = h.reader().tag_chain(&v1_target.landing).unwrap();
        assert_eq!(chain.len(), 6);
        let dump = h.index_dump_for_tests().unwrap();
        let end = h.log_end();
        let root = src.dir.path().join("h");
        drop(h);
        let file = root.join("index.redb");
        {
            let raw = redb::Database::create(&file).unwrap();
            let w = raw.begin_write().unwrap();
            for table in ["landing_depths", "landing_waits"] {
                let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(table);
                assert!(w.delete_table(def).unwrap(), "{table} was there");
                w.open_table(def).unwrap();
            }
            w.commit().unwrap();
        }
        {
            let ix = rdy_store::index::Index::open(&file).unwrap();
            let entries: Vec<IndexEntry> = chain
                .iter()
                .map(|(l, rec)| IndexEntry::LandingDepth {
                    landing: *l,
                    prev: rec.prev,
                })
                .collect();
            if per_commit {
                for (i, e) in entries.iter().enumerate() {
                    ix.apply_durable(std::slice::from_ref(e), end).unwrap();
                    // nothing has a depth until the first landing arrives
                    let got = ix.landing_depth(&v1_target.landing).unwrap();
                    assert_eq!(got, (i + 1 == entries.len()).then_some(6));
                }
            } else {
                ix.apply_durable(&entries, end).unwrap();
            }
        }
        let mut h = Store::open(&root).unwrap();
        assert!(!h.open_report().index_rebuilt);
        assert_eq!(h.index_dump_for_tests().unwrap(), dump, "{per_commit}");
        assert_eq!(
            h.reader().landing_depth(&v1_target.landing).unwrap(),
            Some(6)
        );
        {
            let _c = Capacity::set(depth - 1, objects, edges);
            capacity_err(
                take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap_err(),
                &format!("holder check exceeds {} landing depth", depth - 1),
            );
        }
        {
            let _c = Capacity::set(depth, objects, edges);
            take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap();
        }
        assert_eq!(
            h.reader().landing_depth(&target.landing).unwrap(),
            Some(depth)
        );
        h.check_holder_bounded_for_tests(depth as usize, objects as usize, edges as usize)
            .unwrap();
        let err = h
            .check_holder_bounded_for_tests(depth as usize - 1, objects as usize, edges as usize)
            .unwrap_err();
        let text = format!("{} landing depth", depth - 1);
        assert_eq!(
            rdy_store::check_holder::holder_check_limit(&err),
            Some(text.as_str())
        );
        tcw_same_after_rebuilds(h, &root);
    }
}

/// Lands two changes on the store at `at` (block names from `from`), tags the last `tag`, and
/// writes the batch of its closure less `known` as `name`.
fn tcw_branch(
    src: &Src,
    at: &Path,
    from: usize,
    tag: &str,
    known: &BTreeSet<ObjectKey>,
    name: &str,
) -> (UploadTarget, PathBuf) {
    let mut s = Store::open(at).unwrap();
    let last = tcw_land(&mut s, from, 2);
    let t = tcw_tag(&mut s, tag, last);
    let r = s.reader();
    let c = closure(&r, t, known).unwrap();
    let path = src.dir.path().join(format!("{name}.rdysync"));
    write(&r, &src.repo, t, &c.records, c.prerequisites, &path);
    (
        UploadTarget {
            tag: tag.into(),
            landing: last,
        },
        path,
    )
}

#[test]
fn tcw_a_fork_has_exact_depths_live_after_reindex_and_rebuild() {
    // two tags whose chains part after v1.1's landing: each new first landing names it as prev
    let src = source();
    let mut s = Store::open(&src.path).unwrap();
    let head = tcw_land(&mut s, 0, 3);
    let v1 = tcw_tag(&mut s, "v1.1", head);
    drop(s);
    let (_, mut h) = holder(&src, "h");
    let v1_target = UploadTarget {
        tag: "v1.1".into(),
        landing: head,
    };
    take(&mut h, &open(&src.split(v1, 1, "v1-full")), &v1_target).unwrap();
    let known: BTreeSet<ObjectKey> = src.closure(v1).into_iter().collect();
    let copy = src.dir.path().join("src-fork");
    carried_fixture::copy_store(&src.path, &copy);
    let (a, a_path) = tcw_branch(&src, &src.path, 50_000, "v2", &known, "a");
    let (b, b_path) = tcw_branch(&src, &copy, 60_000, "v3", &known, "b");
    take(&mut h, &open(&[a_path]), &a).unwrap();
    take(&mut h, &open(&[b_path]), &b).unwrap();
    let held = h.reader().tag_chain(&head).unwrap().len() as u64;
    assert_eq!(held, 4);
    let (ca, cb) = (
        h.reader().tag_chain(&a.landing).unwrap(),
        h.reader().tag_chain(&b.landing).unwrap(),
    );
    assert_ne!(ca[1].0, cb[1].0);
    assert_eq!((ca[1].1.prev, cb[1].1.prev), (Some(head), Some(head)));
    let r = h.reader();
    for l in [ca[1].0, cb[1].0] {
        assert_eq!(r.landing_depth(&l).unwrap(), Some(held + 1));
    }
    for l in [a.landing, b.landing] {
        assert_eq!(r.landing_depth(&l).unwrap(), Some(held + 2));
    }
    drop(r);
    h.check_holder_bounded_for_tests(held as usize + 2, 1_000_000, 1_000_000)
        .unwrap();
    let err = h
        .check_holder_bounded_for_tests(held as usize + 1, 1_000_000, 1_000_000)
        .unwrap_err();
    let text = format!("{} landing depth", held + 1);
    assert_eq!(
        rdy_store::check_holder::holder_check_limit(&err),
        Some(text.as_str())
    );
    let root = src.dir.path().join("h");
    let h = tcw_same_after_rebuilds(h, &root);
    assert_eq!(
        h.reader().landing_depth(&b.landing).unwrap(),
        Some(held + 2)
    );
}

/// Appends `items` as one sealed batch to the log of the closed store at `root`, past the
/// store's own writer (which writes a held key once): the next open indexes them as it catches
/// its index up from the log.
fn tcw_raw_append(root: &Path, items: &[(u8, Vec<u8>)]) {
    let (mut log, quarantined) =
        rdy_store::segment::SegmentLog::open_marked(&root.join("segments"), 64 * 1024 * 1024, None)
            .unwrap();
    assert!(quarantined.is_empty());
    log.append_batch(items).unwrap();
    log.sync().unwrap();
}

#[test]
fn tcw_duplicate_frames_charge_their_links_once() {
    // repeated TAG frames, in one batch and across batches, across a replay chunk and a
    // checkpoint, add one object and its links once: live, after a reindex and after a rebuild
    let (src, _, _, mut h, _, _) = tcw_capacity_setup(2);
    let root = src.dir.path().join("h");
    let tag = |name: &str| {
        enc(&Tag {
            name: name.into(),
            landing: src.landing,
            message: None,
            tagged_by: prov("human:alice", Role::Approver),
        })
    };
    let base = h.reader().holder_counts().unwrap();
    h.write(vec![(kinds::TAG, tag("dup-a"))]).unwrap();
    let one = h.reader().holder_counts().unwrap();
    assert_eq!(one.0, base.0 + 1);
    let links = one.1 - base.1;
    assert!(links > 0);
    let t = Hash::of(&tag("dup-a"));
    let mut frame = None;
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |_, f, _| {
            if let StoredFrame::Present {
                kind,
                hash,
                payload,
            } = f
                && hash == t
            {
                frame = Some((kind, payload.to_vec()));
            }
            Ok(())
        })
        .unwrap();
    let frame = frame.expect("the tag's frame");
    assert_eq!(frame.1, tag("dup-a"));
    drop(h);
    // copies: three in one batch, one in each of two batches, then one batch past a replay
    // chunk (a checkpoint too), each indexed by the next open's catch-up
    for n in [3, 1, 1, rdy_store::CATCH_UP_CHUNK + 100] {
        tcw_raw_append(&root, &vec![frame.clone(); n]);
        let h = Store::open(&root).unwrap();
        assert!(!h.open_report().index_rebuilt);
        assert_eq!(h.reader().holder_counts().unwrap(), one, "{n} copies");
    }
    // a key whose first frame has copies in the same batch
    tcw_raw_append(&root, &vec![(frame.0, tag("dup-b")); 5]);
    let h = Store::open(&root).unwrap();
    let two = (one.0 + 1, one.1 + links);
    assert_eq!(h.reader().holder_counts().unwrap(), two);
    h.check_holder().unwrap();
    let h = tcw_same_after_rebuilds(h, &root);
    assert_eq!(h.reader().holder_counts().unwrap(), two);
}

// tcw (known issue 2309): an older rdy (0.47.0 to 0.49.0) writes to the store, moves
// `indexed_upto` and leaves the capacity tables as they were. The index records a second
// position, `counts_upto`, that only a writer that keeps the counts moves; an open that finds
// the two apart rebuilds the index from the log, so no count or depth is read low

/// The capacity tables of the index.
const TCW_COUNT_TABLES: [&str; 3] = ["holder_counts", "landing_depths", "landing_waits"];

/// The rows of the capacity tables and the `counts_upto` value of the index file `file`.
type TcwCounts = (Vec<Vec<(Vec<u8>, Vec<u8>)>>, Option<Vec<u8>>);

fn tcw_read_counts(file: &Path) -> TcwCounts {
    use redb::{ReadableDatabase, ReadableTable};
    let raw = redb::Database::create(file).unwrap();
    let r = raw.begin_read().unwrap();
    let tables = TCW_COUNT_TABLES
        .iter()
        .map(|name| {
            let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(name);
            let t = r.open_table(def).unwrap();
            t.iter()
                .unwrap()
                .map(|item| {
                    let (k, v) = item.unwrap();
                    (k.value().to_vec(), v.value().to_vec())
                })
                .collect()
        })
        .collect();
    let meta: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("meta");
    let upto = r
        .open_table(meta)
        .unwrap()
        .get("counts_upto")
        .unwrap()
        .map(|v| v.value().to_vec());
    (tables, upto)
}

/// Puts the capacity tables and `counts_upto` of the index file `file` back to `was`.
fn tcw_write_counts(file: &Path, was: &TcwCounts) {
    let raw = redb::Database::create(file).unwrap();
    let w = raw.begin_write().unwrap();
    for (name, rows) in TCW_COUNT_TABLES.iter().zip(&was.0) {
        let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(name);
        assert!(w.delete_table(def).unwrap(), "{name} was there");
        let mut t = w.open_table(def).unwrap();
        for (k, v) in rows {
            t.insert(&k[..], &v[..]).unwrap();
        }
    }
    {
        let meta: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("meta");
        let mut t = w.open_table(meta).unwrap();
        match &was.1 {
            Some(v) => t.insert("counts_upto", &v[..]).unwrap(),
            None => t.remove("counts_upto").unwrap(),
        };
    }
    w.commit().unwrap();
}

/// Runs `write` on the closed store at `root` as an older rdy does: the log and every other
/// index table move, `indexed_upto` with them, and the capacity tables and `counts_upto` stay as
/// they were. Asserts that the write moved the capacity tables (so the older writer left them
/// behind).
fn tcw_older_writer(root: &Path, write: impl FnOnce(&mut Store)) {
    let file = root.join("index.redb");
    let was = tcw_read_counts(&file);
    let mut h = Store::open(root).unwrap();
    assert!(!h.open_report().index_rebuilt);
    write(&mut h);
    drop(h);
    assert_ne!(tcw_read_counts(&file).0, was.0, "the write moved no count");
    tcw_write_counts(&file, &was);
}

/// Lands `n` more changes past `src`'s tag (from block `from`) and tags them `tag`; returns the
/// target and the batch of the tag's closure less `known`.
fn tcw_next_tag(
    src: &Src,
    from: usize,
    n: usize,
    known: &BTreeSet<ObjectKey>,
    tag: &str,
) -> (UploadTarget, PathBuf, Hash) {
    let mut s = Store::open(&src.path).unwrap();
    let last = tcw_land(&mut s, from, n);
    let t = tcw_tag(&mut s, tag, last);
    drop(s);
    let (_s, r) = src.reader();
    let c = closure(&r, t, known).unwrap();
    let path = src.dir.path().join(format!("{tag}.rdysync"));
    write(&r, &src.repo, t, &c.records, c.prerequisites, &path);
    (
        UploadTarget {
            tag: tag.into(),
            landing: last,
        },
        path,
        t,
    )
}

#[test]
fn tcw_counts_left_behind_by_an_older_writer_are_rebuilt_at_open() {
    let (src, v1_target, v1_paths, h, target, path) = tcw_capacity_setup(10);
    let (objects, edges, depth) = tcw_after(&src, &v1_target, &v1_paths, &target, &path);
    let root = src.dir.path().join("h");
    drop(h);
    tcw_older_writer(&root, |h| {
        take(h, &open(std::slice::from_ref(&path)), &target).unwrap();
    });
    // counts behind the log are not damage: the index opens, and says its counts are behind
    {
        let ix = rdy_store::index::Index::open(&root.join("index.redb")).unwrap();
        assert!(ix.counts_behind().unwrap(), "the counts are behind the log");
    }
    let mut h = Store::open(&root).unwrap();
    assert!(h.open_report().index_rebuilt);
    assert_eq!(tcw_set_aside(&root), Vec::<String>::new());
    assert_eq!(h.reader().holder_counts().unwrap(), (objects, edges));
    assert_eq!(
        h.reader().landing_depth(&target.landing).unwrap(),
        Some(depth)
    );
    let dump = h.index_dump_for_tests().unwrap();
    h.reindex().unwrap();
    assert_eq!(h.index_dump_for_tests().unwrap(), dump, "reindex");
    h.check_holder().unwrap();
}

#[test]
fn tcw_depth_after_an_older_writer_counts_from_the_true_depth() {
    let (src, _, _, h, target, path) = tcw_capacity_setup(10);
    let root = src.dir.path().join("h");
    drop(h);
    // the older rdy takes v2: its landing's depth is 13, and the index holds no row of it
    tcw_older_writer(&root, |h| {
        take(h, &open(std::slice::from_ref(&path)), &target).unwrap();
    });
    let v2 = tcw_tag_of(&src, "v2");
    let src = Src { tag: v2, ..src };
    let known = tcw_known(&src);
    let (v3_target, v3_path, _) = tcw_next_tag(&src, 60_000, 2, &known, "v3");
    let mut h = Store::open(&root).unwrap();
    // v3 sits two landings past v2: depth 15, which a count from depth 0 would read as 2
    {
        let _c = Capacity::set(14, u64::MAX, u64::MAX);
        capacity_err(
            take(&mut h, &open(std::slice::from_ref(&v3_path)), &v3_target).unwrap_err(),
            "holder check exceeds 14 landing depth",
        );
    }
    {
        let _c = Capacity::set(15, u64::MAX, u64::MAX);
        take(&mut h, &open(std::slice::from_ref(&v3_path)), &v3_target).unwrap();
    }
    assert_eq!(
        h.reader().landing_depth(&v3_target.landing).unwrap(),
        Some(15)
    );
    h.check_holder().unwrap();
}

/// The tag `name` of the source project.
fn tcw_tag_of(src: &Src, name: &str) -> Hash {
    let s = Store::open(&src.path).unwrap();
    let v = s.get_ref(&format!("tag/{name}")).unwrap().expect("tagged");
    Hash(v.try_into().expect("a tag ref is a hash"))
}

#[test]
fn tcw_an_open_after_a_normal_write_does_not_rebuild_the_index() {
    // a holder: takes and reopens
    let (src, _, _, mut h, target, path) = tcw_capacity_setup(3);
    take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap();
    let root = src.dir.path().join("h");
    drop(h);
    for i in 0..3 {
        let mut h = Store::open(&root).unwrap();
        assert!(!h.open_report().index_rebuilt, "holder open {i}");
        // a repeat take is a no-op write; the reindex writes a whole new index
        take(&mut h, &open(std::slice::from_ref(&path)), &target).unwrap();
        if i == 1 {
            h.reindex().unwrap();
        }
    }
    // a project: writes and reopens
    for i in 0..3 {
        let mut s = Store::open(&src.path).unwrap();
        assert!(!s.open_report().index_rebuilt, "project open {i}");
        tcw_land(&mut s, 70_000 + 10 * i, 2);
        if i == 1 {
            s.reindex().unwrap();
        }
    }
    let s = Store::open(&src.path).unwrap();
    assert!(!s.open_report().index_rebuilt, "last project open");
}

#[test]
fn tcw_an_index_without_counts_upto_is_rebuilt_once() {
    // as rdy 0.50.0 writes it: the capacity tables, but no `counts_upto`
    let (src, _, _, mut h, target, path) = tcw_capacity_setup(5);
    take(&mut h, &open(&[path]), &target).unwrap();
    let dump = h.index_dump_for_tests().unwrap();
    let root = src.dir.path().join("h");
    drop(h);
    let file = root.join("index.redb");
    let mut was = tcw_read_counts(&file);
    assert!(was.1.is_some(), "this code writes counts_upto");
    was.1 = None;
    tcw_write_counts(&file, &was);
    let h = Store::open(&root).unwrap();
    assert!(h.open_report().index_rebuilt);
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
    drop(h);
    let h = Store::open(&root).unwrap();
    assert!(!h.open_report().index_rebuilt);
    assert_eq!(h.index_dump_for_tests().unwrap(), dump);
}

/// Bench only (TASK-2324): a take of only the new records of `v2` past about 5,000 held
/// objects, timed up to the caller's check. Prints the times; asserts none.
#[test]
#[ignore]
fn tcw_bench_take_of_new_records_past_5000_held_objects() {
    let (src, mut h) = tcw_held_history(1000);
    let known = tcw_known(&src);
    let (target, path, records, _) = tcw_next(&src, 2, &known, "bench");
    let mut times = Vec::new();
    for _ in 0..7 {
        let start = std::time::Instant::now();
        let mut at = None;
        h.take_batches(
            &open(std::slice::from_ref(&path)),
            &target,
            |_: &mut dyn ObjectSource, _: &UploadFacts| {
                at = Some(start.elapsed());
                Ok::<_, StreamError>(TagChoice::Incoming)
            },
        )
        .unwrap();
        times.push(at.unwrap());
    }
    times.sort();
    println!(
        "tcw bench: {} held objects, {} new; take to the caller's check: median {:?}, min {:?}, max {:?}",
        known.len(),
        records.len(),
        times[3],
        times[0],
        times[6]
    );
}

// tcw (known issue 2309, review round): counts behind the log are not damage. The open trusts
// the index, so its acknowledged end stays the log's floor, and the rebuild from the log zeroes
// the old index file as a removal's reindex does, so no copy of what a pending removal erases is
// kept

/// The names of the index files an open set aside as damaged in the store at `root`.
fn tcw_set_aside(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index.redb.damaged-"))
        .collect();
    out.sort();
    out
}

/// Sets `counts_upto` of the closed store at `root` to `to` (None removes it), as rdy 0.50.0
/// (missing) or an older writer after this code (stale) leaves it; the capacity tables stay.
fn tcw_set_counts_upto(root: &Path, to: Option<Vec<u8>>) {
    let file = root.join("index.redb");
    let mut was = tcw_read_counts(&file);
    assert!(was.1.is_some(), "this code writes counts_upto");
    was.1 = to;
    tcw_write_counts(&file, &was);
}

/// The names of the files in the segments folder of the store at `root`.
fn tcw_segment_names(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(root.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn tcw_counts_behind_keep_the_acknowledged_floor() {
    const BLOB: &[u8] = b"tcw acknowledged blob under a damaged marker";
    // counts_upto kept (the control), missing (0.50.0) and stale (an older writer)
    for (what, counts_upto) in [
        ("kept", None),
        ("missing", Some(None)),
        ("stale", Some(Some(enc(&ORIGIN)))),
    ] {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("s");
        let blob = {
            let mut s = Store::create(&root).unwrap();
            let blob = s.put_blob(BLOB).unwrap();
            s.close().unwrap();
            blob
        };
        if let Some(to) = counts_upto {
            tcw_set_counts_upto(&root, to);
        }
        // the final batch's marker is damaged: only the index's floor says the batch was
        // acknowledged
        let seg = root
            .join("segments")
            .join(tcw_segment_names(&root).last().unwrap());
        let mut bytes = std::fs::read(&seg).unwrap();
        *bytes.last_mut().unwrap() ^= 0xff;
        std::fs::write(&seg, &bytes).unwrap();
        let (segments, names) = (segment_bytes(&root), tcw_segment_names(&root));
        let e = Store::open(&root)
            .err()
            .unwrap_or_else(|| panic!("{what}: the open truncated an acknowledged batch"));
        assert!(
            matches!(&e, rdy_store::StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
            "{what}: {e:?}"
        );
        assert!(
            e.to_string()
                .contains("damages acknowledged data; refusing to truncate"),
            "{what}: {e}"
        );
        assert_eq!(
            segment_bytes(&root),
            segments,
            "{what}: the log is as it was"
        );
        assert_eq!(
            tcw_segment_names(&root),
            names,
            "{what}: nothing quarantined"
        );
        assert_eq!(tcw_set_aside(&root), Vec::<String>::new(), "{what}");
        // the marker restored, the acknowledged blob reads back
        *bytes.last_mut().unwrap() ^= 0xff;
        std::fs::write(&seg, &bytes).unwrap();
        let s = Store::open(&root).unwrap();
        assert_eq!(
            s.open_report().index_rebuilt,
            what != "kept",
            "{what}: counts behind the log are rebuilt"
        );
        assert_eq!(s.get_blob(&blob).unwrap(), BLOB, "{what}");
        s.close().unwrap();
        assert_eq!(tcw_set_aside(&root), Vec::<String>::new(), "{what}");
    }
}

/// The text of the block a removal erases in the tcw removal test.
const TCW_SECRET: &str = "tcw canary removal secret";

/// A diagram cache row that holds [`TCW_SECRET`], as a drawing of the block caches it.
fn tcw_secret_row() -> (Vec<u8>, Vec<u8>) {
    let mut key = vec![0xAB; 32];
    key.extend_from_slice(b"mmdflux 2.6.1\0test");
    let payload = TCW_SECRET.as_bytes();
    let mut value = vec![1u8];
    value.extend_from_slice(&Hash::of(payload).0);
    value.extend_from_slice(payload);
    (key, value)
}

/// The files under `root` that hold [`TCW_SECRET`].
fn tcw_files_with_secret(root: &Path) -> Vec<PathBuf> {
    let secret = TCW_SECRET.as_bytes();
    let mut found = vec![];
    let mut files = vec![root.to_path_buf()];
    while let Some(p) = files.pop() {
        if p.is_dir() {
            files.extend(std::fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
        } else if std::fs::read(&p)
            .unwrap()
            .windows(secret.len())
            .any(|w| w == secret)
        {
            found.push(p);
        }
    }
    found.sort();
    found
}

#[test]
fn tcw_counts_behind_with_a_pending_removal_keep_no_copy_of_the_removed_text() {
    use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
    let secret = block("b-00009", TCW_SECRET);
    // counts_upto missing (0.50.0) and stale (an older writer)
    for (what, counts_upto) in [("missing", None), ("stale", Some(enc(&ORIGIN)))] {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("s");
        {
            let mut s = Store::create(&root).unwrap();
            s.commit_to_view(
                "work",
                change(
                    "work",
                    vec![Op::Create {
                        block: secret.clone(),
                    }],
                ),
            )
            .unwrap();
            let (key, value) = tcw_secret_row();
            s.reader().diagram_cache().put_raw_for_tests(&key, &value);
            s.checkpoint().unwrap();
            s.close().unwrap();
        }
        // an authorized removal of the block, not finished: the next open finishes it
        let f = authorized_batch_fixture(
            &root,
            &[RequestSpec::new(
                "work",
                vec![rdy_model::redaction::RedactTarget::Version {
                    id: secret.id.clone(),
                    version: secret.version_hash(),
                }],
            )],
            &FixtureOptions::approved(Proof::local_human()),
        );
        tcw_set_counts_upto(&root, counts_upto);
        assert!(
            tcw_files_with_secret(&root).contains(&root.join("index.redb")),
            "{what}: the index holds the cached drawing"
        );
        let s = Store::open(&root).unwrap();
        assert!(s.open_report().index_rebuilt, "{what}");
        assert_eq!(s.reader().diagram_cache().rows_for_tests(), 0, "{what}");
        assert_eq!(
            s.get_block(&secret.version_hash()).unwrap().redaction(),
            Some(f.requests[0].request),
            "{what}: the removal finished"
        );
        s.close().unwrap();
        assert_eq!(
            tcw_files_with_secret(&root),
            Vec::<PathBuf>::new(),
            "{what}: no file holds the removed text"
        );
        assert_eq!(tcw_set_aside(&root), Vec::<String>::new(), "{what}");
        // and the next open does not rebuild again
        let s = Store::open(&root).unwrap();
        assert!(!s.open_report().index_rebuilt, "{what}: rebuilt once");
        s.close().unwrap();
    }
}
